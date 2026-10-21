//! Persistent ticket state for the branch processor.
//!
//! A ticket mirrors one wallet-created mint or melt quote at the mint. Tickets
//! are created when cdk-mintd relays a wallet's quote request over gRPC and are
//! settled when the operator confirms the cash movement in the teller UI
//! (matched by quote id). All tickets are stored in a single JSON file on disk.
//! The file is fully rewritten on every mutation (atomic rename), which is more
//! than fine at the scale this is built for (manual branch ops, dozens of
//! tickets per day at most).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use cdk_common::nuts::{CurrencyUnit, MeltQuoteState};
use cdk_common::payment::{Event, MakePaymentResponse, PaymentIdentifier, WaitPaymentResponse};
use cdk_common::{Amount, QuoteId};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

/// Upper bound on concurrently open (unsettled) tickets. Quote creation is
/// unauthenticated at the mint, so this caps how much a hostile wallet can
/// grow the store and the teller's open-quote list before the expiry sweep
/// reclaims the slots.
pub const MAX_OPEN_QUOTES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketKind {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    /// Outgoing only: the melt quote exists but the wallet has not locked
    /// funds yet — the operator must NOT pay out in this state.
    Waiting,
    /// Incoming: open quote awaiting the customer's cash at the counter.
    /// Outgoing: funded by the wallet (proofs locked at the mint), safe to pay out.
    Pending,
    Paid,
    /// Voided by the operator or unresolvable. Also the landing state for
    /// legacy `offered` rows written by the removed quote-offer flow.
    #[serde(alias = "offered")]
    Failed,
}

impl TicketStatus {
    pub fn is_active(self) -> bool {
        matches!(self, TicketStatus::Waiting | TicketStatus::Pending)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    /// The mint's quote id for this ticket. Always set for tickets created by
    /// the current flow (the ticket id is derived from it); `None` only on
    /// legacy rows, which are voided at load because they cannot be matched.
    #[serde(default)]
    pub quote_id: Option<String>,
    pub kind: TicketKind,
    pub amount: u64,
    pub unit: String,
    pub status: TicketStatus,
    pub created_at: u64,
    pub paid_at: Option<u64>,
    pub description: Option<String>,
    /// Free-form notes added by the operator.
    pub notes: Option<String>,
    /// Unix timestamp after which an unsettled ticket is dead. Mint tickets
    /// mirror the mint's quote expiry (reported over gRPC); melt tickets use
    /// a local bookkeeping window (the mint's own melt-quote TTL governs the
    /// wallet).
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// When the ticket was voided (paid tickets carry `paid_at` instead).
    #[serde(default)]
    pub voided_at: Option<u64>,
    /// The console user who settled or voided the ticket at the counter.
    #[serde(default)]
    pub operator: Option<String>,
}

impl Ticket {
    /// A wallet-created mint quote awaiting cash at the counter.
    pub fn new_incoming(
        quote_id: String,
        amount: u64,
        unit: String,
        description: Option<String>,
        expires_at: Option<u64>,
    ) -> Self {
        Self {
            id: format!("MINT-{quote_id}"),
            quote_id: Some(quote_id),
            kind: TicketKind::Incoming,
            amount,
            unit,
            status: TicketStatus::Pending,
            created_at: unix_now(),
            paid_at: None,
            description,
            notes: None,
            expires_at,
            voided_at: None,
            operator: None,
        }
    }

    /// A wallet-created melt quote; payout stays blocked until the wallet
    /// locks its proofs at the mint (`mark_outgoing_submitted`).
    pub fn new_outgoing(
        quote_id: String,
        amount: u64,
        unit: String,
        description: Option<String>,
        expires_at: Option<u64>,
    ) -> Self {
        Self {
            id: format!("MELT-{quote_id}"),
            quote_id: Some(quote_id),
            kind: TicketKind::Outgoing,
            amount,
            unit,
            status: TicketStatus::Waiting,
            created_at: unix_now(),
            paid_at: None,
            description,
            notes: None,
            expires_at,
            voided_at: None,
            operator: None,
        }
    }

    pub fn expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|e| now > e)
    }

    /// When the ticket left the open list: settled, voided, or (for rows
    /// that predate `voided_at`) created. Orders and dates the history.
    pub fn closed_at(&self) -> u64 {
        self.paid_at.or(self.voided_at).unwrap_or(self.created_at)
    }

    pub fn unit_typed(&self) -> CurrencyUnit {
        self.unit
            .parse()
            .unwrap_or_else(|_| CurrencyUnit::Custom(self.unit.as_str().into()))
    }
}

/// Normalized teller input for quote matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchQuery {
    /// A full quote id (scanned): compare hex-equal against the whole id.
    Exact(String),
    /// A typed tail of the quote id, ≥ 6 hex characters.
    Suffix(String),
}

/// Normalize raw teller input (typed tail or scanned full id) into a match
/// query. Tolerates surrounding whitespace and CR/LF appended by keyboard-wedge
/// scanners, mixed case, hyphens typed or omitted, and a leading `MINT-`/`MELT-`
/// ticket prefix (in case a wallet renders the payment request string instead
/// of the bare quote id).
pub fn normalize_match_input(raw: &str) -> Result<MatchQuery, String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let without_prefix = trimmed
        .strip_prefix("mint-")
        .or_else(|| trimmed.strip_prefix("melt-"))
        .unwrap_or(&trimmed);
    let hex: String = without_prefix.chars().filter(|c| *c != '-').collect();
    if hex.is_empty() {
        return Err("enter the last 6+ characters of the quote id".to_string());
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(
            "that doesn't look like a quote id — scan or type the id itself, not a link"
                .to_string(),
        );
    }
    match hex.len() {
        32 => Ok(MatchQuery::Exact(hex)),
        6..=31 => Ok(MatchQuery::Suffix(hex)),
        _ if hex.len() < 6 => Err("enter at least the last 6 characters of the quote id".into()),
        _ => Err("that is longer than a quote id — scan again".into()),
    }
}

/// Outcome of matching teller input against open tickets.
#[derive(Debug)]
pub enum MatchResult {
    Unique(Ticket),
    /// No open ticket matched. `inactive_match` is true when a settled or
    /// voided ticket matches the code, so the UI can hint at that without
    /// revealing which one.
    None { inactive_match: bool },
    Ambiguous(usize),
}

fn hex_normalized(id: &str) -> String {
    id.chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn query_matches(query: &MatchQuery, ticket: &Ticket) -> bool {
    let Some(quote_id) = ticket.quote_id.as_deref() else {
        return false;
    };
    let quote_hex = hex_normalized(quote_id);
    match query {
        MatchQuery::Exact(hex) => quote_hex == *hex,
        MatchQuery::Suffix(suffix) => quote_hex.ends_with(suffix.as_str()),
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    tickets: BTreeMap<String, Ticket>,
}

/// Shared, mutable state of the branch processor.
#[derive(Clone)]
pub struct BranchState {
    inner: Arc<Inner>,
}

struct Inner {
    tickets: RwLock<BTreeMap<String, Ticket>>,
    path: PathBuf,
    /// Broadcast so the mint can reconnect (`wait_payment_event` again) without
    /// us having to coordinate "stream already taken" semantics.
    event_tx: broadcast::Sender<Event>,
    /// Coarse "something changed" notifier for the web UI's SSE stream.
    ui_changed_tx: broadcast::Sender<()>,
}

impl std::fmt::Debug for BranchState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BranchState").finish_non_exhaustive()
    }
}

impl BranchState {
    pub async fn load(path: PathBuf) -> Result<Self> {
        let mut tickets = if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            let raw = tokio::fs::read(&path)
                .await
                .with_context(|| format!("read {}", path.display()))?;
            match serde_json::from_slice::<StateFile>(&raw) {
                Ok(file) => {
                    if raw.windows(9).any(|w| w == b"\"offered\"") {
                        tracing::warn!(
                            "tickets.json contains legacy offer tickets from the removed \
                             quote-offer flow; they were voided"
                        );
                    }
                    file.tickets
                }
                Err(e) => {
                    // Never silently discard the settlement record: move the
                    // unreadable file aside and start empty.
                    let quarantine = path.with_extension(format!("json.corrupt-{}", unix_now()));
                    tokio::fs::rename(&path, &quarantine)
                        .await
                        .with_context(|| format!("quarantine {}", path.display()))?;
                    tracing::error!(
                        "tickets.json is unreadable ({e}); moved it to {} and starting empty",
                        quarantine.display()
                    );
                    BTreeMap::new()
                }
            }
        } else {
            BTreeMap::new()
        };
        // Legacy open tickets without a quote id cannot be matched by the
        // teller (the offer flow attached quote ids late or not at all) —
        // void them rather than leaving unsettleable rows in the list.
        for ticket in tickets.values_mut() {
            if ticket.status.is_active() && ticket.quote_id.is_none() {
                tracing::warn!(
                    "voiding legacy ticket {} on load: no quote id, cannot be matched",
                    ticket.id
                );
                ticket.status = TicketStatus::Failed;
                ticket.notes = Some("voided on upgrade: created by the removed offer flow".into());
            }
        }
        let (tx, _rx) = broadcast::channel(256);
        let (ui_tx, _ui_rx) = broadcast::channel(256);
        Ok(Self {
            inner: Arc::new(Inner {
                tickets: RwLock::new(tickets),
                path,
                event_tx: tx,
                ui_changed_tx: ui_tx,
            }),
        })
    }

    pub async fn list_all(&self) -> Vec<Ticket> {
        self.inner.tickets.read().await.values().cloned().collect()
    }

    /// Look up one ticket by its id (e.g. `MINT-<quote_id>`). Used by the
    /// self-test to confirm a probe quote landed on THIS processor.
    pub async fn get_ticket(&self, id: &str) -> Option<Ticket> {
        self.inner.tickets.read().await.get(id).cloned()
    }

    /// All unsettled tickets. Currently exercised by tests; kept as the
    /// natural query for future console features.
    #[allow(dead_code)]
    pub async fn active_tickets(&self) -> Vec<Ticket> {
        self.inner
            .tickets
            .read()
            .await
            .values()
            .filter(|t| t.status.is_active())
            .cloned()
            .collect()
    }

    /// Register a wallet-created quote. Multiple open tickets are normal (one
    /// per waiting customer); `MAX_OPEN_QUOTES` bounds hostile growth.
    ///
    /// Re-registering the same id is allowed only as a transport-level retry:
    /// the existing ticket must still be open and describe the same quote.
    /// Anything else is a replay of a used quote id and is rejected.
    pub async fn insert_open(&self, ticket: Ticket) -> Result<Ticket> {
        let updated = {
            let mut t = self.inner.tickets.write().await;
            if let Some(existing) = t.get(&ticket.id) {
                if existing.status.is_active()
                    && existing.kind == ticket.kind
                    && existing.amount == ticket.amount
                    && existing.unit == ticket.unit
                {
                    return Ok(existing.clone());
                }
                bail!("quote id {} has already been used", ticket.id);
            }
            let open = t.values().filter(|x| x.status.is_active()).count();
            if open >= MAX_OPEN_QUOTES {
                bail!("too many open quotes ({open}); try again later");
            }
            t.insert(ticket.id.clone(), ticket.clone());
            ticket
        };
        self.persist().await?;
        self.notify_ui_change();
        Ok(updated)
    }

    /// Match teller input against open tickets by quote id.
    pub async fn match_open(&self, query: &MatchQuery) -> MatchResult {
        let tickets = self.inner.tickets.read().await;
        let mut open = tickets
            .values()
            .filter(|t| t.status.is_active() && query_matches(query, t));
        let first = open.next().cloned();
        let extra = open.count();
        match (first, extra) {
            (Some(ticket), 0) => MatchResult::Unique(ticket),
            (Some(_), n) => MatchResult::Ambiguous(n + 1),
            (None, _) => {
                let inactive_match = tickets
                    .values()
                    .any(|t| !t.status.is_active() && query_matches(query, t));
                MatchResult::None { inactive_match }
            }
        }
    }

    /// Find the outgoing ticket for a mint melt quote id.
    pub async fn outgoing_by_quote_id(&self, quote_id: &str) -> Option<Ticket> {
        self.inner
            .tickets
            .read()
            .await
            .values()
            .find(|t| t.kind == TicketKind::Outgoing && t.quote_id.as_deref() == Some(quote_id))
            .cloned()
    }

    /// Move a melt ticket into the state where customer proofs are locked by
    /// the mint and the operator can safely dispense cash.
    pub async fn mark_outgoing_submitted(&self, id: &str) -> Result<Ticket> {
        let mut changed = false;
        let updated = {
            let mut t = self.inner.tickets.write().await;
            let ticket = t
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("unknown outgoing quote {id}"))?;
            if ticket.kind != TicketKind::Outgoing {
                bail!("ticket {id} is not an outgoing quote");
            }
            if ticket.status == TicketStatus::Failed {
                bail!("melt quote {id} was voided or expired; it cannot be funded");
            }
            if ticket.status == TicketStatus::Waiting {
                ticket.status = TicketStatus::Pending;
                changed = true;
            }
            ticket.clone()
        };
        if changed {
            self.persist().await?;
            self.notify_ui_change();
        }
        Ok(updated)
    }

    /// Mark a ticket as paid after the operator confirmed the cash movement.
    ///
    /// Incoming: pushes a `PaymentReceived` event into the mint's
    /// `wait_payment_event` stream so the mint flips the mint quote to PAID
    /// without polling. Outgoing: pushes `PaymentSuccessful` so the customer's
    /// wallet sees the melt finalize immediately instead of on its next poll.
    /// Events fire only on an actual `Pending → Paid` transition — a repeated
    /// confirm is a no-op and a voided ticket is rejected outright.
    /// `operator` is the console user confirming, recorded for the history.
    pub async fn mark_paid(
        &self,
        id: &str,
        notes: Option<String>,
        operator: Option<String>,
    ) -> Result<Ticket> {
        let (updated, transitioned) = {
            let mut t = self.inner.tickets.write().await;
            let ticket = t
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("ticket {id} not found"))?;
            match ticket.status {
                TicketStatus::Waiting => {
                    bail!("ticket {id} is waiting for the wallet to lock funds")
                }
                TicketStatus::Failed => bail!("ticket {id} was voided; it cannot be settled"),
                TicketStatus::Paid => (ticket.clone(), false),
                TicketStatus::Pending => {
                    if ticket.kind == TicketKind::Incoming && ticket.expired(unix_now()) {
                        bail!(
                            "quote {id} has expired; ask the customer to create a fresh one \
                             before taking cash"
                        );
                    }
                    ticket.status = TicketStatus::Paid;
                    ticket.paid_at = Some(unix_now());
                    ticket.operator = operator;
                    if let Some(n) = notes {
                        ticket.notes = Some(n);
                    }
                    (ticket.clone(), true)
                }
            }
        };
        self.persist().await?;

        if transitioned {
            match updated.kind {
                TicketKind::Incoming => {
                    let amount = Amount::new(updated.amount, updated.unit_typed());
                    let event = Event::PaymentReceived(WaitPaymentResponse {
                        payment_identifier: PaymentIdentifier::CustomId(updated.id.clone()),
                        payment_amount: amount,
                        payment_id: format!("{}-receipt", updated.id),
                    });
                    // best-effort — without a subscriber the mint picks it up via
                    // check_incoming_payment_status on the wallet's next poll
                    let _ = self.inner.event_tx.send(event);
                }
                TicketKind::Outgoing => {
                    if let Some(event) = outgoing_settled_event(&updated) {
                        let _ = self.inner.event_tx.send(event);
                    }
                }
            }
        }

        self.notify_ui_change();
        Ok(updated)
    }

    /// Void a ticket. For a funded melt (proofs locked at the mint) a
    /// `PaymentFailed` event is pushed so the mint releases the customer's
    /// proofs immediately.
    pub async fn mark_failed(
        &self,
        id: &str,
        notes: Option<String>,
        operator: Option<String>,
    ) -> Result<Ticket> {
        let (updated, was_funded_melt) = {
            let mut t = self.inner.tickets.write().await;
            let ticket = t
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("ticket {id} not found"))?;
            if ticket.status == TicketStatus::Paid {
                bail!("ticket {id} is already settled");
            }
            let was_funded_melt =
                ticket.kind == TicketKind::Outgoing && ticket.status == TicketStatus::Pending;
            if ticket.status != TicketStatus::Failed {
                ticket.status = TicketStatus::Failed;
                ticket.voided_at = Some(unix_now());
                ticket.operator = operator;
                if let Some(n) = notes {
                    ticket.notes = Some(n);
                }
            }
            (ticket.clone(), was_funded_melt)
        };
        self.persist().await?;
        if was_funded_melt {
            if let Some(quote_id) = parse_quote_id(&updated) {
                let _ = self.inner.event_tx.send(Event::PaymentFailed {
                    quote_id,
                    reason: updated
                        .notes
                        .clone()
                        .unwrap_or_else(|| "voided at the counter".to_string()),
                });
            }
        }
        self.notify_ui_change();
        Ok(updated)
    }

    /// Remove expired tickets no money ever moved for: open mint quotes the
    /// customer never completed and melt quotes the wallet never funded.
    /// Funded melts (proofs locked) are never touched — only the operator or
    /// the wallet may resolve those. Runs in a single write pass so a melt
    /// funded concurrently cannot slip through mid-sweep.
    pub async fn sweep_expired(&self) -> usize {
        let removed = {
            let mut t = self.inner.tickets.write().await;
            let now = unix_now();
            let expired: Vec<String> = t
                .values()
                .filter(|ticket| {
                    matches!(
                        (ticket.kind, ticket.status),
                        (TicketKind::Incoming, TicketStatus::Pending)
                            | (TicketKind::Outgoing, TicketStatus::Waiting)
                    ) && ticket.expired(now)
                })
                .map(|ticket| ticket.id.clone())
                .collect();
            for id in &expired {
                t.remove(id);
            }
            expired.len()
        };
        if removed > 0 {
            if let Err(e) = self.persist().await {
                tracing::error!("persist after sweep_expired: {e}");
            }
            self.notify_ui_change();
        }
        removed
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.inner.event_tx.subscribe()
    }

    pub fn subscribe_ui_changes(&self) -> broadcast::Receiver<()> {
        self.inner.ui_changed_tx.subscribe()
    }

    pub fn notify_ui_change(&self) {
        let _ = self.inner.ui_changed_tx.send(());
    }

    /// Look up an incoming ticket by its payment identifier and translate to
    /// the WaitPaymentResponse the mint expects from `check_incoming_payment_status`.
    pub async fn lookup_incoming(&self, id: &PaymentIdentifier) -> Vec<WaitPaymentResponse> {
        let ticket_id = match id {
            PaymentIdentifier::CustomId(s) => s.clone(),
            _ => return Vec::new(),
        };
        let tickets = self.inner.tickets.read().await;
        let Some(t) = tickets.get(&ticket_id) else {
            return Vec::new();
        };
        if t.kind != TicketKind::Incoming || t.status != TicketStatus::Paid {
            return Vec::new();
        }
        vec![WaitPaymentResponse {
            payment_identifier: PaymentIdentifier::CustomId(t.id.clone()),
            payment_amount: Amount::new(t.amount, t.unit_typed()),
            payment_id: format!("{}-receipt", t.id),
        }]
    }

    /// Same as `lookup_incoming` but for outgoing (melt) tickets — returns a
    /// MakePaymentResponse the mint uses to advance the melt quote.
    pub async fn lookup_outgoing(&self, id: &PaymentIdentifier) -> Option<MakePaymentResponse> {
        let ticket_id = match id {
            PaymentIdentifier::CustomId(s) => s.clone(),
            _ => return None,
        };
        let tickets = self.inner.tickets.read().await;
        let t = tickets.get(&ticket_id)?;
        if t.kind != TicketKind::Outgoing {
            return None;
        }
        Some(outgoing_payment_response(t))
    }

    async fn persist(&self) -> Result<()> {
        let tickets = self.inner.tickets.read().await.clone();
        let file = StateFile { tickets };
        let bytes = serde_json::to_vec_pretty(&file)?;
        let tmp = self.inner.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, &bytes).await?;
        tokio::fs::rename(&tmp, &self.inner.path).await?;
        Ok(())
    }
}

fn outgoing_payment_response(t: &Ticket) -> MakePaymentResponse {
    let status = match t.status {
        TicketStatus::Paid => MeltQuoteState::Paid,
        TicketStatus::Pending => MeltQuoteState::Pending,
        TicketStatus::Waiting => MeltQuoteState::Unpaid,
        TicketStatus::Failed => MeltQuoteState::Failed,
    };
    MakePaymentResponse {
        payment_lookup_id: PaymentIdentifier::CustomId(t.id.clone()),
        payment_proof: t.notes.clone(),
        status,
        total_spent: Amount::new(t.amount, t.unit_typed()),
    }
}

/// The `PaymentSuccessful` event for a settled melt, if its quote id parses.
fn outgoing_settled_event(t: &Ticket) -> Option<Event> {
    let quote_id = parse_quote_id(t)?;
    Some(Event::PaymentSuccessful {
        quote_id,
        details: outgoing_payment_response(t),
    })
}

fn parse_quote_id(t: &Ticket) -> Option<QuoteId> {
    let raw = t.quote_id.as_deref()?;
    match QuoteId::from_str(raw) {
        Ok(quote_id) => Some(quote_id),
        Err(e) => {
            tracing::warn!("ticket {} has an unparseable quote id ({e})", t.id);
            None
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh_state() -> BranchState {
        let path =
            std::env::temp_dir().join(format!("branch-state-test-{}.json", uuid::Uuid::new_v4()));
        BranchState::load(path).await.expect("load state")
    }

    fn incoming(quote_id: &str, amount: u64) -> Ticket {
        Ticket::new_incoming(quote_id.into(), amount, "ora".into(), None, None)
    }

    fn outgoing(quote_id: &str, amount: u64) -> Ticket {
        Ticket::new_outgoing(quote_id.into(), amount, "ora".into(), None, None)
    }

    #[test]
    fn normalizes_match_input() {
        assert_eq!(
            normalize_match_input("  9EC0F4\r\n"),
            Ok(MatchQuery::Suffix("9ec0f4".into()))
        );
        assert_eq!(
            normalize_match_input("2f4-b6e"),
            Ok(MatchQuery::Suffix("2f4b6e".into()))
        );
        let full = "0198c0ef-3f11-7abc-9def-0123456789ab";
        assert_eq!(
            normalize_match_input(full),
            Ok(MatchQuery::Exact(full.replace('-', "")))
        );
        assert_eq!(
            normalize_match_input(&format!("MINT-{full}")),
            Ok(MatchQuery::Exact(full.replace('-', "")))
        );
        assert!(normalize_match_input("12345").is_err()); // too short
        assert!(normalize_match_input("https://mint/quote/abc123").is_err()); // not an id
        assert!(normalize_match_input("").is_err());
        assert!(normalize_match_input(&"f".repeat(40)).is_err()); // longer than an id
    }

    #[tokio::test]
    async fn suffix_matching_finds_unique_open_quote() {
        let state = fresh_state().await;
        state
            .insert_open(incoming("0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4", 500))
            .await
            .unwrap();
        state
            .insert_open(incoming("0198c0ef-3f11-7abc-9def-bbbbbb111111", 700))
            .await
            .unwrap();

        let query = normalize_match_input("9EC0F4").unwrap();
        match state.match_open(&query).await {
            MatchResult::Unique(t) => assert_eq!(t.amount, 500),
            other => panic!("expected unique match, got {other:?}"),
        }

        // full scanned id (with hyphens) matches exactly
        let query = normalize_match_input("0198c0ef-3f11-7abc-9def-bbbbbb111111").unwrap();
        match state.match_open(&query).await {
            MatchResult::Unique(t) => assert_eq!(t.amount, 700),
            other => panic!("expected unique match, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ambiguous_and_missing_suffixes_are_reported() {
        let state = fresh_state().await;
        state
            .insert_open(incoming("0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4", 500))
            .await
            .unwrap();
        state
            .insert_open(outgoing("0198c0ef-3f11-7abc-9def-bbbbbb9ec0f4", 200))
            .await
            .unwrap();

        let query = normalize_match_input("9ec0f4").unwrap();
        assert!(matches!(
            state.match_open(&query).await,
            MatchResult::Ambiguous(2)
        ));

        // longer suffix disambiguates
        let query = normalize_match_input("bbb9ec0f4").unwrap();
        assert!(matches!(
            state.match_open(&query).await,
            MatchResult::Unique(_)
        ));

        let query = normalize_match_input("dddddd").unwrap();
        assert!(matches!(
            state.match_open(&query).await,
            MatchResult::None {
                inactive_match: false
            }
        ));
    }

    #[tokio::test]
    async fn settled_quotes_hint_but_do_not_match() {
        let state = fresh_state().await;
        let t = state
            .insert_open(incoming("0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4", 500))
            .await
            .unwrap();
        state.mark_paid(&t.id, None, None).await.unwrap();

        let query = normalize_match_input("9ec0f4").unwrap();
        assert!(matches!(
            state.match_open(&query).await,
            MatchResult::None {
                inactive_match: true
            }
        ));
    }

    #[tokio::test]
    async fn incoming_settles_and_emits_event_once() {
        let state = fresh_state().await;
        let t = state
            .insert_open(incoming("0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4", 500))
            .await
            .unwrap();
        let mut rx = state.subscribe_events();

        let paid = state
            .mark_paid(&t.id, Some("till #1".into()), Some("teller".into()))
            .await
            .unwrap();
        assert_eq!(paid.status, TicketStatus::Paid);
        assert_eq!(paid.operator.as_deref(), Some("teller"));
        assert_eq!(paid.closed_at(), paid.paid_at.unwrap());
        assert!(matches!(rx.try_recv(), Ok(Event::PaymentReceived(_))));

        // repeated confirm is a no-op and must not double-credit
        let again = state.mark_paid(&t.id, None, None).await.unwrap();
        assert_eq!(again.status, TicketStatus::Paid);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn voided_and_expired_tickets_cannot_be_settled() {
        let state = fresh_state().await;
        let t = state
            .insert_open(incoming("0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4", 500))
            .await
            .unwrap();
        state.mark_failed(&t.id, None, None).await.unwrap();
        let mut rx = state.subscribe_events();
        assert!(state.mark_paid(&t.id, None, None).await.is_err());
        assert!(rx.try_recv().is_err(), "voided ticket must not emit payment");

        let mut expired = incoming("0198c0ef-3f11-7abc-9def-bbbbbb111111", 700);
        expired.expires_at = Some(unix_now() - 1);
        let expired = state.insert_open(expired).await.unwrap();
        assert!(state.mark_paid(&expired.id, None, None).await.is_err());
    }

    #[tokio::test]
    async fn quote_id_replay_is_rejected() {
        let state = fresh_state().await;
        let quote = "0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4";
        let t = state.insert_open(incoming(quote, 500)).await.unwrap();

        // transport-level retry with identical parameters echoes the ticket
        let retry = state.insert_open(incoming(quote, 500)).await.unwrap();
        assert_eq!(retry.id, t.id);

        // same id with different parameters is a replay
        assert!(state.insert_open(incoming(quote, 999)).await.is_err());

        // settled ids can never be re-registered
        state.mark_paid(&t.id, None, None).await.unwrap();
        assert!(state.insert_open(incoming(quote, 500)).await.is_err());
    }

    #[tokio::test]
    async fn open_quote_cap_is_enforced() {
        let state = fresh_state().await;
        for i in 0..MAX_OPEN_QUOTES {
            state
                .insert_open(incoming(&format!("0198c0ef-3f11-7abc-9def-{i:012x}"), 1))
                .await
                .unwrap();
        }
        assert!(state
            .insert_open(incoming("0198c0ef-3f11-7abc-9def-ffffffffffff", 1))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn outgoing_lifecycle_gates_payout() {
        let state = fresh_state().await;
        let t = state
            .insert_open(outgoing("0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4", 700))
            .await
            .unwrap();
        assert_eq!(t.status, TicketStatus::Waiting);

        // wallet has not locked funds → no payout
        assert!(state.mark_paid(&t.id, None, None).await.is_err());

        // wallet funds the melt → Pending, payout allowed, success event fires
        let funded = state.mark_outgoing_submitted(&t.id).await.unwrap();
        assert_eq!(funded.status, TicketStatus::Pending);
        let mut rx = state.subscribe_events();
        let paid = state.mark_paid(&t.id, None, None).await.unwrap();
        assert_eq!(paid.status, TicketStatus::Paid);
        assert!(matches!(
            rx.try_recv(),
            Ok(Event::PaymentSuccessful { .. })
        ));
        assert!(state
            .outgoing_by_quote_id("0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4")
            .await
            .is_some_and(|x| x.id == t.id));
    }

    #[tokio::test]
    async fn voided_melt_cannot_be_funded_and_funded_void_notifies() {
        let state = fresh_state().await;
        let t = state
            .insert_open(outgoing("0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4", 700))
            .await
            .unwrap();
        state.mark_failed(&t.id, None, None).await.unwrap();
        assert!(state.mark_outgoing_submitted(&t.id).await.is_err());

        let f = state
            .insert_open(outgoing("0198c0ef-3f11-7abc-9def-bbbbbb111111", 300))
            .await
            .unwrap();
        state.mark_outgoing_submitted(&f.id).await.unwrap();
        let mut rx = state.subscribe_events();
        let voided = state
            .mark_failed(&f.id, Some("customer left".into()), Some("teller".into()))
            .await
            .unwrap();
        assert!(matches!(rx.try_recv(), Ok(Event::PaymentFailed { .. })));
        assert_eq!(voided.operator.as_deref(), Some("teller"));
        assert!(voided.voided_at.is_some());
        assert_eq!(voided.closed_at(), voided.voided_at.unwrap());
    }

    #[tokio::test]
    async fn sweep_deletes_expired_unfunded_only() {
        let state = fresh_state().await;
        let past = unix_now() - 10;

        let mut dead_mint = incoming("0198c0ef-3f11-7abc-9def-aaaaaa111111", 10);
        dead_mint.expires_at = Some(past);
        let mut dead_melt = outgoing("0198c0ef-3f11-7abc-9def-bbbbbb222222", 20);
        dead_melt.expires_at = Some(past);
        let mut funded_melt = outgoing("0198c0ef-3f11-7abc-9def-cccccc333333", 30);
        funded_melt.expires_at = Some(past);
        let live = incoming("0198c0ef-3f11-7abc-9def-dddddd444444", 40);

        for t in [
            dead_mint.clone(),
            dead_melt.clone(),
            funded_melt.clone(),
            live.clone(),
        ] {
            state.insert_open(t).await.unwrap();
        }
        state
            .mark_outgoing_submitted(&funded_melt.id)
            .await
            .unwrap();

        assert_eq!(state.sweep_expired().await, 2);
        let remaining: Vec<String> = state
            .active_tickets()
            .await
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert!(remaining.contains(&funded_melt.id));
        assert!(remaining.contains(&live.id));
        assert!(!remaining.contains(&dead_mint.id));
        assert!(!remaining.contains(&dead_melt.id));
    }

    #[tokio::test]
    async fn legacy_state_file_loads_and_voids_unmatchable_rows() {
        let path = std::env::temp_dir().join(format!(
            "branch-state-legacy-{}.json",
            uuid::Uuid::new_v4()
        ));
        // Verbatim shape written by the removed offer flow: an unclaimed offer,
        // a claimed (pending) incoming without a quote id, a claimed melt with
        // one, and a settled row.
        let legacy = r#"{
          "tickets": {
            "MINT-11111111-aaaa-bbbb-cccc-dddddddddddd": {
              "id": "MINT-11111111-aaaa-bbbb-cccc-dddddddddddd",
              "quote_id": null,
              "kind": "incoming",
              "amount": 500,
              "unit": "ora",
              "status": "offered",
              "created_at": 1700000000,
              "paid_at": null,
              "description": null,
              "notes": null,
              "offer": "cquoteAexample",
              "expires_at": 1700000900
            },
            "MINT-22222222-aaaa-bbbb-cccc-dddddddddddd": {
              "id": "MINT-22222222-aaaa-bbbb-cccc-dddddddddddd",
              "quote_id": null,
              "kind": "incoming",
              "amount": 700,
              "unit": "ora",
              "status": "pending",
              "created_at": 1700000000,
              "paid_at": null,
              "description": null,
              "notes": null,
              "expires_at": null
            },
            "MELT-33333333-aaaa-bbbb-cccc-dddddddddddd": {
              "id": "MELT-33333333-aaaa-bbbb-cccc-dddddddddddd",
              "quote_id": "0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4",
              "kind": "outgoing",
              "amount": 200,
              "unit": "ora",
              "status": "waiting",
              "created_at": 1700000000,
              "paid_at": null,
              "description": null,
              "notes": null,
              "expires_at": null
            },
            "MINT-44444444-aaaa-bbbb-cccc-dddddddddddd": {
              "id": "MINT-44444444-aaaa-bbbb-cccc-dddddddddddd",
              "quote_id": null,
              "kind": "incoming",
              "amount": 900,
              "unit": "ora",
              "status": "paid",
              "created_at": 1700000000,
              "paid_at": 1700000500,
              "description": null,
              "notes": "receipt",
              "expires_at": null
            }
          }
        }"#;
        tokio::fs::write(&path, legacy).await.unwrap();

        let state = BranchState::load(path).await.unwrap();
        let all = state.list_all().await;
        assert_eq!(all.len(), 4, "no rows may be dropped on upgrade");

        let by_id = |suffix: &str| {
            all.iter()
                .find(|t| t.id.starts_with(suffix))
                .cloned()
                .unwrap()
        };
        // unclaimed offer → Failed via the serde alias
        assert_eq!(by_id("MINT-1111").status, TicketStatus::Failed);
        // active legacy row without quote id → voided at load
        assert_eq!(by_id("MINT-2222").status, TicketStatus::Failed);
        // claimed melt kept: it has a quote id and stays matchable
        assert_eq!(by_id("MELT-3333").status, TicketStatus::Waiting);
        // settled history untouched
        let settled = by_id("MINT-4444");
        assert_eq!(settled.status, TicketStatus::Paid);
        assert_eq!(settled.notes.as_deref(), Some("receipt"));
    }

    #[tokio::test]
    async fn corrupt_state_file_is_quarantined_not_wiped() {
        let path = std::env::temp_dir().join(format!(
            "branch-state-corrupt-{}.json",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::write(&path, b"{ definitely not json").await.unwrap();

        let state = BranchState::load(path.clone()).await.unwrap();
        assert!(state.list_all().await.is_empty());
        assert!(
            !tokio::fs::try_exists(&path).await.unwrap(),
            "corrupt file must be moved aside, not left in place"
        );

        let dir = path.parent().unwrap();
        let mut quarantined = false;
        let prefix = path.file_stem().unwrap().to_string_lossy().to_string();
        let mut entries = tokio::fs::read_dir(dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && name.contains("corrupt-") {
                quarantined = true;
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
        assert!(quarantined, "corrupt file must be preserved under a new name");
    }
}
