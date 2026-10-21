//! Attachment checklist and end-to-end self-test.
//!
//! The processor never configures the attached mint; instead it verifies the
//! mint's observable surfaces and tells the operator exactly what to fix.
//! `evaluate` turns raw signals (mint `/v1/info`, `/v1/keysets`, gRPC attach
//! timestamps, the last self-test) into five plain-language checks rendered
//! by the console's Mint tab. `run_self_test` acts as a wallet for one round
//! trip per direction — it creates a real branch mint quote and melt quote at
//! the mint, confirms the resulting tickets landed on THIS processor (with
//! the PR #2295 quote id + NUT-20 pubkey), then voids them.

use serde::Serialize;
use serde_json::Value;

use crate::clients::{KeysetEntry, MintHttpClient};
use crate::config::{compatible_mintd_image, COMPATIBLE_CDK_VERSION};
use crate::state::{BranchState, Ticket};
use crate::timefmt::iso_date;

/// Notes marker on tickets created (and immediately voided) by the
/// self-test; the web layer filters them out of every operator-facing list.
pub const SELF_TEST_NOTE: &str = "self-test";

/// Description the self-test puts on its quotes, so they are recognizable in
/// the mint's records too.
pub const SELF_TEST_DESCRIPTION: &str = "Connection self-test — safe to ignore";

/// Warn when the mint's deposit-quote lifetime is shorter than this: the
/// customer has to reach the counter before it runs out.
const MIN_COMFORTABLE_MINT_TTL_SECS: u64 = 900;
/// Warn when the melt-quote lifetime is shorter than this (cdk's default of
/// 60 s trips wallets that show a confirmation screen).
const MIN_COMFORTABLE_MELT_TTL_SECS: u64 = 300;
/// Warn when the active keyset's ecash stops being redeemable within this
/// window: the mint operator needs time to rotate, wallets time to move.
pub const KEYSET_EXPIRY_WARNING_SECS: u64 = 30 * 86_400;

pub fn is_self_test_ticket(ticket: &Ticket) -> bool {
    ticket.notes.as_deref() == Some(SELF_TEST_NOTE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    /// Cannot be evaluated yet (setup incomplete, or an upstream check failed).
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub status: CheckStatus,
    pub title: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

pub struct ChecklistInputs<'a> {
    pub now: u64,
    pub unit: &'a str,
    pub method: &'a str,
    pub mint_url: &'a str,
    /// `None` when no mint is attached (the probe never ran).
    pub info: Option<&'a anyhow::Result<Value>>,
    pub keysets: Option<&'a anyhow::Result<Vec<KeysetEntry>>>,
    pub last_settings_at: Option<u64>,
    pub stream_attached_at: Option<u64>,
    pub self_test: Option<&'a SelfTestOutcome>,
}

pub fn evaluate(inputs: &ChecklistInputs) -> Vec<Check> {
    vec![
        check_reachable(inputs),
        check_advertised(inputs),
        check_linked(inputs),
        check_keyset(inputs),
        check_end_to_end(inputs),
    ]
}

fn check_reachable(inputs: &ChecklistInputs) -> Check {
    let id = "reachable";
    let title = "Mint is reachable";
    if inputs.mint_url.is_empty() {
        return Check {
            id,
            status: CheckStatus::Unknown,
            title,
            detail: "No mint attached yet — set the mint URL in setup.".into(),
            remedy: None,
        };
    }
    match inputs.info {
        Some(Ok(info)) => {
            let identity = mint_identity(info);
            let mut parts = Vec::new();
            if let Some(name) = identity.name {
                parts.push(name);
            }
            if let Some(version) = identity.version {
                parts.push(version);
            }
            let detail = if parts.is_empty() {
                "Responding.".to_string()
            } else {
                format!("Responding: {}.", parts.join(" · "))
            };
            Check {
                id,
                status: CheckStatus::Ok,
                title,
                detail,
                remedy: None,
            }
        }
        Some(Err(e)) => Check {
            id,
            status: CheckStatus::Fail,
            title,
            detail: format!("Could not reach {}: {e:#}.", inputs.mint_url),
            remedy: Some(
                "Check the URL — it must be the mint's public API base, the same URL wallets \
                 use. Confirm the mint is running and reachable from this host."
                    .into(),
            ),
        },
        None => Check {
            id,
            status: CheckStatus::Unknown,
            title,
            detail: "Not probed.".into(),
            remedy: None,
        },
    }
}

fn check_advertised(inputs: &ChecklistInputs) -> Check {
    let id = "advertised";
    let title = "Branch payments advertised";
    if inputs.unit.is_empty() || inputs.mint_url.is_empty() {
        return Check {
            id,
            status: CheckStatus::Unknown,
            title,
            detail: "Complete setup first.".into(),
            remedy: None,
        };
    }
    let Some(Ok(info)) = inputs.info else {
        return Check {
            id,
            status: CheckStatus::Unknown,
            title,
            detail: "Waiting for the mint to be reachable.".into(),
            remedy: None,
        };
    };
    let summary = advertised_for_method(info, inputs.method);
    let ours = summary
        .pairs
        .iter()
        .find(|pair| pair.unit == inputs.unit);
    let extra_units: Vec<&str> = summary
        .pairs
        .iter()
        .filter(|pair| pair.unit != inputs.unit)
        .map(|pair| pair.unit.as_str())
        .collect();

    let pinned_remedy = format!(
        "Add the config snippet below to your mint's stored configuration and restart it: \
         cdk-mintd config export --file mint.toml, merge the snippet, cdk-mintd config \
         apply --file mint.toml, then restart. On cdk-mintd {version}+ the stored \
         configuration is authoritative — editing a mint.toml on disk without `config \
         apply` changes nothing. If the {method}/{unit} entry is already applied and this \
         check still fails, check the mint's log for why the backend was skipped.",
        version = COMPATIBLE_CDK_VERSION,
        unit = inputs.unit,
        method = inputs.method,
    );

    match ours {
        Some(pair) if pair.mint && pair.melt => {
            if extra_units.is_empty() {
                Check {
                    id,
                    status: CheckStatus::Ok,
                    title,
                    detail: format!(
                        "The mint advertises {} deposits and payouts for {}.",
                        inputs.method, inputs.unit
                    ),
                    remedy: None,
                }
            } else {
                Check {
                    id,
                    status: CheckStatus::Warn,
                    title,
                    detail: format!(
                        "Advertised for {}, but the mint also advertises {} for {} — this \
                         processor serves only {}, so those quotes will fail.",
                        inputs.unit,
                        inputs.method,
                        extra_units.join(", "),
                        inputs.unit
                    ),
                    remedy: Some(format!(
                        "Remove the extra [[payment_backend]] entries for {} from the \
                         mint's stored config (config export, edit, config apply, restart).",
                        extra_units.join(", ")
                    )),
                }
            }
        }
        Some(pair) => {
            let missing = match (pair.mint, pair.melt) {
                (false, true) => "deposits (NUT-04)",
                (true, false) => "payouts (NUT-05)",
                _ => "deposits and payouts",
            };
            Check {
                id,
                status: CheckStatus::Warn,
                title,
                detail: format!(
                    "The mint advertises {}/{} only partially — {} missing{}.",
                    inputs.unit,
                    inputs.method,
                    missing,
                    disabled_note(&summary),
                ),
                remedy: Some(pinned_remedy),
            }
        }
        None => Check {
            id,
            status: CheckStatus::Fail,
            title,
            detail: format!(
                "The mint does not advertise {} for {}{}.",
                inputs.method,
                inputs.unit,
                disabled_note(&summary),
            ),
            remedy: Some(pinned_remedy),
        },
    }
}

fn disabled_note(summary: &AdvertisedSummary) -> String {
    match (summary.nut04_disabled, summary.nut05_disabled) {
        (true, true) => " (the mint reports both minting and melting disabled)".into(),
        (true, false) => " (the mint reports minting disabled)".into(),
        (false, true) => " (the mint reports melting disabled)".into(),
        (false, false) => String::new(),
    }
}

fn check_linked(inputs: &ChecklistInputs) -> Check {
    let id = "linked";
    let title = "Mint linked to this processor";
    match (inputs.stream_attached_at, inputs.last_settings_at) {
        (Some(_), _) => Check {
            id,
            status: CheckStatus::Ok,
            title,
            detail: "The mint is attached to the payment stream.".into(),
            remedy: None,
        },
        (None, Some(_)) => Check {
            id,
            status: CheckStatus::Warn,
            title,
            detail: "The mint read this processor's settings but never attached to the \
                     payment stream — its startup did not finish."
                .into(),
            remedy: Some(
                "Check the mint's log: a unit mismatch between its [[payment_backend]] \
                 entry and the unit configured here aborts its startup."
                    .into(),
            ),
        },
        (None, None) => {
            if inputs.unit.is_empty() {
                Check {
                    id,
                    status: CheckStatus::Unknown,
                    title,
                    detail: "Complete setup first — the mint cannot start against an \
                             unconfigured processor."
                        .into(),
                    remedy: None,
                }
            } else {
                Check {
                    id,
                    status: CheckStatus::Fail,
                    title,
                    detail: "No mint has connected to this processor since it started.".into(),
                    remedy: Some(format!(
                        "Check [grpc_processor] address/port in the mint's config, the network \
                         path between the two, and that the mint runs cdk-mintd \
                         v{version} or later (docker image {image}) — the payment-processor \
                         protocol check (protocol {proto}) rejects older builds at connect \
                         time; the rejection appears in the mint's log, not here. Then \
                         restart the mint.",
                        version = COMPATIBLE_CDK_VERSION,
                        image = compatible_mintd_image(),
                        proto = cdk_common::PAYMENT_PROCESSOR_PROTOCOL_VERSION,
                    )),
                }
            }
        }
    }
}

fn check_keyset(inputs: &ChecklistInputs) -> Check {
    let id = "keyset";
    let title = "Unit has active keys";
    if inputs.unit.is_empty() || inputs.mint_url.is_empty() {
        return Check {
            id,
            status: CheckStatus::Unknown,
            title,
            detail: "Complete setup first.".into(),
            remedy: None,
        };
    }
    match inputs.keysets {
        Some(Ok(keysets)) => {
            let of_unit: Vec<&KeysetEntry> = keysets
                .iter()
                .filter(|keyset| keyset.unit == inputs.unit)
                .collect();
            if let Some(active) = of_unit.iter().find(|keyset| keyset.active) {
                active_keyset_check(active, inputs.now)
            } else if !of_unit.is_empty() {
                Check {
                    id,
                    status: CheckStatus::Warn,
                    title,
                    detail: format!(
                        "The mint has {} keyset(s) for {} but none is active — new deposits \
                         cannot be issued.",
                        of_unit.len(),
                        inputs.unit
                    ),
                    remedy: Some(
                        "Rotate a fresh keyset with the mint's own tooling (cdk-mint-cli \
                         rotate-next-keyset), or restart the mint so it ensures one."
                            .into(),
                    ),
                }
            } else {
                Check {
                    id,
                    status: CheckStatus::Fail,
                    title,
                    detail: format!("The mint has no keys for {} yet.", inputs.unit),
                    remedy: Some(
                        "The mint creates keys for the unit on its first start with the \
                         [[payment_backend]] entry from the snippet — apply the snippet \
                         (cdk-mintd config apply) and restart the mint."
                            .into(),
                    ),
                }
            }
        }
        Some(Err(e)) => Check {
            id,
            status: CheckStatus::Unknown,
            title,
            detail: format!("Could not read the mint's keyset list: {e:#}."),
            remedy: None,
        },
        None => Check {
            id,
            status: CheckStatus::Unknown,
            title,
            detail: "Waiting for the mint to be reachable.".into(),
            remedy: None,
        },
    }
}

/// The active keyset's state, expiry included: an expired active keyset
/// blocks issuance (cdk refuses to sign with it) and mintd does not replace
/// it on restart, so it fails; an imminent expiry warns.
fn active_keyset_check(active: &KeysetEntry, now: u64) -> Check {
    let id = "keyset";
    let title = "Unit has active keys";
    let rotate = "Rotate to a fresh keyset with the mint's own tooling (cdk-mint-cli \
                  rotate-next-keyset) — the Keys tab has the exact command.";
    match active.final_expiry {
        // cdk treats a keyset as expired strictly after its final expiry.
        Some(expiry) if expiry < now => Check {
            id,
            status: CheckStatus::Fail,
            title,
            detail: format!(
                "The active keyset {} expired on {} — the mint can no longer issue \
                 ecash for this unit, so deposits cannot complete.",
                active.id,
                iso_date(expiry)
            ),
            remedy: Some(format!(
                "{rotate} Restarting the mint does not replace an expired keyset."
            )),
        },
        Some(expiry) if expiry - now < KEYSET_EXPIRY_WARNING_SECS => Check {
            id,
            status: CheckStatus::Warn,
            title,
            detail: format!(
                "The active keyset {} expires on {} ({} days) — ecash still on it then \
                 can no longer be redeemed.",
                active.id,
                iso_date(expiry),
                (expiry - now) / 86_400
            ),
            remedy: Some(rotate.to_string()),
        },
        expiry => Check {
            id,
            status: CheckStatus::Ok,
            title,
            detail: format!(
                "Active keyset {} (fee {} ppk, {}).",
                active.id,
                active.input_fee_ppk,
                match expiry {
                    Some(expiry) => format!("expires {}", iso_date(expiry)),
                    None => "no expiry".to_string(),
                }
            ),
            remedy: None,
        },
    }
}

fn check_end_to_end(inputs: &ChecklistInputs) -> Check {
    let id = "end_to_end";
    let title = "End-to-end test";
    match inputs.self_test {
        None => Check {
            id,
            status: CheckStatus::Unknown,
            title,
            detail: "Not run yet. It runs automatically once the mint links up, or run the \
                     self-test from the Mint tab."
                .into(),
            remedy: None,
        },
        Some(outcome) => {
            let mut details = vec![outcome.deposit.detail.clone(), outcome.payout.detail.clone()];
            details.extend(outcome.warnings.iter().cloned());
            let detail = details.join(" ");
            if !outcome.ok {
                let remedy = outcome
                    .deposit
                    .remedy
                    .clone()
                    .or_else(|| outcome.payout.remedy.clone());
                Check {
                    id,
                    status: CheckStatus::Fail,
                    title,
                    detail,
                    remedy,
                }
            } else if !outcome.warnings.is_empty() {
                Check {
                    id,
                    status: CheckStatus::Warn,
                    title,
                    detail,
                    remedy: None,
                }
            } else {
                Check {
                    id,
                    status: CheckStatus::Ok,
                    title,
                    detail,
                    remedy: None,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// /v1/info parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedPair {
    pub unit: String,
    pub mint: bool,
    pub melt: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AdvertisedSummary {
    pub pairs: Vec<AdvertisedPair>,
    pub nut04_disabled: bool,
    pub nut05_disabled: bool,
}

/// The (unit → mint/melt) pairs the mint advertises for one payment method,
/// from NUT-04 and NUT-05 settings in `/v1/info`.
pub fn advertised_for_method(info: &Value, method: &str) -> AdvertisedSummary {
    let mut summary = AdvertisedSummary::default();
    let mut pairs = std::collections::BTreeMap::<String, (bool, bool)>::new();
    for (nut, is_mint) in [("4", true), ("5", false)] {
        let settings = info.get("nuts").and_then(|nuts| nuts.get(nut));
        let disabled = settings
            .and_then(|settings| settings.get("disabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_mint {
            summary.nut04_disabled = disabled;
        } else {
            summary.nut05_disabled = disabled;
        }
        let methods = settings
            .and_then(|settings| settings.get("methods"))
            .and_then(Value::as_array);
        for entry in methods.into_iter().flatten() {
            let Some(entry_method) = entry.get("method").and_then(Value::as_str) else {
                continue;
            };
            if entry_method != method {
                continue;
            }
            let Some(unit) = entry.get("unit").and_then(Value::as_str) else {
                continue;
            };
            let slot = pairs.entry(unit.to_string()).or_insert((false, false));
            if is_mint && !disabled {
                slot.0 = true;
            }
            if !is_mint && !disabled {
                slot.1 = true;
            }
        }
    }
    summary.pairs = pairs
        .into_iter()
        .map(|(unit, (mint, melt))| AdvertisedPair { unit, mint, melt })
        .collect();
    summary
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MintIdentity {
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub version: Option<String>,
}

pub fn mint_identity(info: &Value) -> MintIdentity {
    let get = |key: &str| {
        info.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    MintIdentity {
        name: get("name"),
        description: get("description"),
        icon_url: get("icon_url"),
        version: get("version"),
    }
}

// ---------------------------------------------------------------------------
// Self-test
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SelfTestLeg {
    pub ok: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

impl SelfTestLeg {
    fn fail(detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            ok: false,
            detail: detail.into(),
            remedy: Some(remedy.into()),
        }
    }

    fn skipped(reason: &str) -> Self {
        Self {
            ok: false,
            detail: format!("Payout test skipped ({reason})."),
            remedy: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SelfTestOutcome {
    pub ran_at: u64,
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub deposit: SelfTestLeg,
    pub payout: SelfTestLeg,
    pub mint_quote_ttl_secs: Option<u64>,
    pub melt_quote_ttl_secs: Option<u64>,
    /// Non-fatal observations (short quote lifetimes).
    pub warnings: Vec<String>,
}

/// One wallet-shaped round trip per direction against the attached mint.
/// Each leg creates a real quote (1 unit), verifies the resulting ticket
/// landed on THIS processor, and voids it immediately. The quotes stay
/// unpaid at the mint and expire on their own.
pub async fn run_self_test(
    mint: &MintHttpClient,
    branch: &BranchState,
    method: &str,
    unit: &str,
) -> SelfTestOutcome {
    let now = unix_now();
    let mut warnings = Vec::new();
    let mut mint_quote_ttl = None;
    let mut melt_quote_ttl = None;
    let mut latency_ms = None;

    // Deposit leg: NUT-20-locked mint quote with an ephemeral key.
    let pubkey = cdk_common::nuts::SecretKey::generate()
        .public_key()
        .to_string();
    let started = std::time::Instant::now();
    let deposit = match mint
        .create_probe_mint_quote(method, unit, 1, &pubkey, SELF_TEST_DESCRIPTION)
        .await
    {
        Err(e) => SelfTestLeg::fail(
            format!("Deposit test: could not reach the mint ({e:#})."),
            "Confirm the mint URL and that the mint is running.",
        ),
        Ok(Err(rejection)) => SelfTestLeg::fail(
            format!("Deposit test: the mint rejected the quote ({rejection})."),
            probe_rejection_remedy(&rejection, unit, method),
        ),
        Ok(Ok(probe)) => {
            latency_ms = Some(started.elapsed().as_millis() as u64);
            mint_quote_ttl = probe.expiry.map(|expiry| expiry.saturating_sub(now));
            let ticket_id = format!("MINT-{}", probe.quote);
            match branch.get_ticket(&ticket_id).await {
                None => SelfTestLeg::fail(
                    "Deposit test: the mint created the quote, but this processor never \
                     received it — the mint is pointed at a different processor instance."
                        .to_string(),
                    "Make sure the [grpc_processor] address in the mint's config points at \
                     THIS processor, then restart the mint.",
                ),
                Some(_) if probe.request != ticket_id => SelfTestLeg::fail(
                    format!(
                        "Deposit test: the mint echoed payment request {} for quote {}, but \
                         this processor issued {} — another processor answered the mint.",
                        probe.request, probe.quote, ticket_id
                    ),
                    "Make sure the [grpc_processor] address in the mint's config points at \
                     THIS processor, then restart the mint.",
                ),
                Some(_) => {
                    if let Err(e) = branch
                        .mark_failed(&ticket_id, Some(SELF_TEST_NOTE.to_string()), None)
                        .await
                    {
                        tracing::warn!("could not void self-test ticket {ticket_id}: {e:#}");
                    }
                    if probe.pubkey.as_deref() != Some(pubkey.as_str()) {
                        warnings.push(
                            "The mint did not echo the NUT-20 pubkey on the quote — check its \
                             build."
                                .into(),
                        );
                    }
                    SelfTestLeg {
                        ok: true,
                        detail: "Deposit test passed: quote created at the mint, quote id and \
                                 NUT-20 lock arrived here, ticket registered and voided."
                            .into(),
                        remedy: None,
                    }
                }
            }
        }
    };

    // Payout leg — only meaningful if the mint was reachable at all.
    let payout = if !deposit.ok && deposit.detail.contains("could not reach") {
        SelfTestLeg::skipped("mint unreachable")
    } else {
        match mint
            .create_probe_melt_quote(method, unit, 1, SELF_TEST_DESCRIPTION)
            .await
        {
            Err(e) => SelfTestLeg::fail(
                format!("Payout test: could not reach the mint ({e:#})."),
                "Confirm the mint URL and that the mint is running.",
            ),
            Ok(Err(rejection)) => SelfTestLeg::fail(
                format!("Payout test: the mint rejected the quote ({rejection})."),
                probe_rejection_remedy(&rejection, unit, method),
            ),
            Ok(Ok(probe)) => {
                melt_quote_ttl = probe.expiry.map(|expiry| expiry.saturating_sub(now));
                let ticket_id = format!("MELT-{}", probe.quote);
                match branch.get_ticket(&ticket_id).await {
                    None => SelfTestLeg::fail(
                        "Payout test: the mint created the melt quote, but this processor \
                         never received it — the mint is pointed at a different processor \
                         instance."
                            .to_string(),
                        "Make sure the [grpc_processor] address in the mint's config points \
                         at THIS processor, then restart the mint.",
                    ),
                    Some(_) => {
                        if let Err(e) = branch
                            .mark_failed(&ticket_id, Some(SELF_TEST_NOTE.to_string()), None)
                            .await
                        {
                            tracing::warn!("could not void self-test ticket {ticket_id}: {e:#}");
                        }
                        SelfTestLeg {
                            ok: true,
                            detail: "Payout test passed: melt quote created and mirrored \
                                     here, ticket voided."
                                .into(),
                            remedy: None,
                        }
                    }
                }
            }
        }
    };

    if let Some(ttl) = mint_quote_ttl {
        if ttl < MIN_COMFORTABLE_MINT_TTL_SECS {
            warnings.push(format!(
                "Deposit quotes expire after {} — tight for a counter visit; set \
                 [info.quote_ttl] mint_ttl = 1800 in the mint's stored config \
                 (cdk-mintd config apply) and restart it.",
                humanize_secs(ttl)
            ));
        }
    }
    if let Some(ttl) = melt_quote_ttl {
        if ttl < MIN_COMFORTABLE_MELT_TTL_SECS {
            warnings.push(format!(
                "Payout quotes expire after {} — wallets showing a confirmation screen will \
                 miss the window; set [info.quote_ttl] melt_ttl = 900 in the mint's stored \
                 config (cdk-mintd config apply) and restart it.",
                humanize_secs(ttl)
            ));
        }
    }

    SelfTestOutcome {
        ran_at: now,
        ok: deposit.ok && payout.ok,
        latency_ms,
        deposit,
        payout,
        mint_quote_ttl_secs: mint_quote_ttl,
        melt_quote_ttl_secs: melt_quote_ttl,
        warnings,
    }
}

fn probe_rejection_remedy(
    rejection: &crate::clients::MintHttpError,
    unit: &str,
    method: &str,
) -> String {
    if rejection.status >= 500 {
        format!(
            "The mint accepted the request but its call to this processor failed. Check the \
             mint's log; usual causes are a wrong [grpc_processor] address, a TLS mismatch, \
             or a mint older than cdk v{COMPATIBLE_CDK_VERSION} (protocol {proto}).",
            proto = cdk_common::PAYMENT_PROCESSOR_PROTOCOL_VERSION,
        )
    } else {
        format!(
            "Usually this means {unit}/{method} is not advertised by the mint — see the \
             \"Branch payments advertised\" check and apply the config snippet."
        )
    }
}

fn humanize_secs(secs: u64) -> String {
    if secs >= 120 {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
    }
}

fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_800_000_000;

    fn info_with(methods4: Value, methods5: Value) -> Value {
        json!({
            "name": "Test Mint",
            "version": "cdk-mintd/0.18.0",
            "nuts": {
                "4": { "methods": methods4, "disabled": false },
                "5": { "methods": methods5, "disabled": false },
            }
        })
    }

    fn base_inputs<'a>(
        info: Option<&'a anyhow::Result<Value>>,
        keysets: Option<&'a anyhow::Result<Vec<KeysetEntry>>>,
    ) -> ChecklistInputs<'a> {
        ChecklistInputs {
            now: NOW,
            unit: "ora",
            method: "branch",
            mint_url: "http://mint:8089",
            info,
            keysets,
            last_settings_at: None,
            stream_attached_at: None,
            self_test: None,
        }
    }

    fn status_of(checks: &[Check], id: &str) -> CheckStatus {
        checks.iter().find(|c| c.id == id).expect(id).status
    }

    #[test]
    fn unattached_setup_reports_unknowns() {
        let inputs = ChecklistInputs {
            now: NOW,
            unit: "",
            mint_url: "",
            method: "branch",
            info: None,
            keysets: None,
            last_settings_at: None,
            stream_attached_at: None,
            self_test: None,
        };
        let checks = evaluate(&inputs);
        assert_eq!(checks.len(), 5);
        for check in &checks {
            assert_eq!(check.status, CheckStatus::Unknown, "{}", check.id);
        }
    }

    #[test]
    fn fully_advertised_pair_is_ok() {
        let info: anyhow::Result<Value> = Ok(info_with(
            json!([{ "method": "branch", "unit": "ora", "min_amount": 1, "max_amount": 500000 }]),
            json!([{ "method": "branch", "unit": "ora" }]),
        ));
        let checks = evaluate(&base_inputs(Some(&info), None));
        assert_eq!(status_of(&checks, "reachable"), CheckStatus::Ok);
        assert_eq!(status_of(&checks, "advertised"), CheckStatus::Ok);
        // gRPC never attached → linked fails with the required-release remedy.
        let linked = checks.iter().find(|c| c.id == "linked").unwrap();
        assert_eq!(linked.status, CheckStatus::Fail);
        let remedy = linked.remedy.as_ref().unwrap();
        assert!(remedy.contains(COMPATIBLE_CDK_VERSION));
        assert!(remedy.contains(&compatible_mintd_image()));
    }

    #[test]
    fn missing_and_partial_advertisement() {
        let missing: anyhow::Result<Value> = Ok(info_with(json!([]), json!([])));
        let checks = evaluate(&base_inputs(Some(&missing), None));
        let advertised = checks.iter().find(|c| c.id == "advertised").unwrap();
        assert_eq!(advertised.status, CheckStatus::Fail);
        assert!(advertised.remedy.as_ref().unwrap().contains("mint.toml"));
        assert!(advertised.remedy.as_ref().unwrap().contains("config apply"));

        let partial: anyhow::Result<Value> = Ok(info_with(
            json!([]),
            json!([{ "method": "branch", "unit": "ora" }]),
        ));
        let checks = evaluate(&base_inputs(Some(&partial), None));
        let advertised = checks.iter().find(|c| c.id == "advertised").unwrap();
        assert_eq!(advertised.status, CheckStatus::Warn);
        assert!(advertised.detail.contains("deposits (NUT-04)"));
    }

    #[test]
    fn extra_units_warn_and_disabled_flag_counts_as_missing() {
        let extra: anyhow::Result<Value> = Ok(info_with(
            json!([
                { "method": "branch", "unit": "ora" },
                { "method": "branch", "unit": "usd" },
            ]),
            json!([
                { "method": "branch", "unit": "ora" },
                { "method": "branch", "unit": "usd" },
            ]),
        ));
        let checks = evaluate(&base_inputs(Some(&extra), None));
        let advertised = checks.iter().find(|c| c.id == "advertised").unwrap();
        assert_eq!(advertised.status, CheckStatus::Warn);
        assert!(advertised.detail.contains("usd"));

        let disabled: anyhow::Result<Value> = Ok(json!({
            "nuts": {
                "4": { "methods": [{ "method": "branch", "unit": "ora" }], "disabled": true },
                "5": { "methods": [{ "method": "branch", "unit": "ora" }], "disabled": false },
            }
        }));
        let checks = evaluate(&base_inputs(Some(&disabled), None));
        let advertised = checks.iter().find(|c| c.id == "advertised").unwrap();
        assert_eq!(advertised.status, CheckStatus::Warn);
        assert!(advertised.detail.contains("minting disabled"));
    }

    #[test]
    fn linked_states() {
        let mut inputs = base_inputs(None, None);
        inputs.last_settings_at = Some(100);
        let checks = evaluate(&inputs);
        assert_eq!(status_of(&checks, "linked"), CheckStatus::Warn);

        inputs.stream_attached_at = Some(120);
        let checks = evaluate(&inputs);
        assert_eq!(status_of(&checks, "linked"), CheckStatus::Ok);
    }

    #[test]
    fn keyset_states() {
        let entry = |active: bool| KeysetEntry {
            id: "00abcd".into(),
            unit: "ora".into(),
            active,
            input_fee_ppk: 0,
            final_expiry: None,
        };
        let active: anyhow::Result<Vec<KeysetEntry>> = Ok(vec![entry(true)]);
        let checks = evaluate(&base_inputs(None, Some(&active)));
        assert_eq!(status_of(&checks, "keyset"), CheckStatus::Ok);

        let inactive: anyhow::Result<Vec<KeysetEntry>> = Ok(vec![entry(false)]);
        let checks = evaluate(&base_inputs(None, Some(&inactive)));
        assert_eq!(status_of(&checks, "keyset"), CheckStatus::Warn);

        let none: anyhow::Result<Vec<KeysetEntry>> = Ok(vec![]);
        let checks = evaluate(&base_inputs(None, Some(&none)));
        assert_eq!(status_of(&checks, "keyset"), CheckStatus::Fail);
    }

    #[test]
    fn active_keyset_expiry() {
        let expiring = |final_expiry: u64| -> anyhow::Result<Vec<KeysetEntry>> {
            Ok(vec![KeysetEntry {
                id: "01abcd".into(),
                unit: "ora".into(),
                active: true,
                input_fee_ppk: 0,
                final_expiry: Some(final_expiry),
            }])
        };
        let far = expiring(NOW + 365 * 86_400);
        let checks = evaluate(&base_inputs(None, Some(&far)));
        assert_eq!(status_of(&checks, "keyset"), CheckStatus::Ok);

        let soon = expiring(NOW + 10 * 86_400);
        let checks = evaluate(&base_inputs(None, Some(&soon)));
        let keyset = checks.iter().find(|c| c.id == "keyset").unwrap();
        assert_eq!(keyset.status, CheckStatus::Warn);
        assert!(keyset.detail.contains("10 days"));

        // Expiry is strict: the keyset still works at exactly its expiry.
        let now = expiring(NOW);
        let checks = evaluate(&base_inputs(None, Some(&now)));
        assert_eq!(status_of(&checks, "keyset"), CheckStatus::Warn);

        let past = expiring(NOW - 1);
        let checks = evaluate(&base_inputs(None, Some(&past)));
        assert_eq!(status_of(&checks, "keyset"), CheckStatus::Fail);
    }

    #[test]
    fn end_to_end_reflects_the_last_outcome() {
        let ok_leg = SelfTestLeg {
            ok: true,
            detail: "Deposit test passed.".into(),
            remedy: None,
        };
        let outcome = SelfTestOutcome {
            ran_at: 1,
            ok: true,
            latency_ms: Some(20),
            deposit: ok_leg.clone(),
            payout: SelfTestLeg {
                detail: "Payout test passed.".into(),
                ..ok_leg.clone()
            },
            mint_quote_ttl_secs: Some(1800),
            melt_quote_ttl_secs: Some(60),
            warnings: vec!["Payout quotes expire after 60 s".into()],
        };
        let failed = SelfTestOutcome {
            ok: false,
            warnings: Vec::new(),
            payout: SelfTestLeg::fail("Payout test: the mint rejected the quote.", "Apply the snippet."),
            ..outcome.clone()
        };

        let mut inputs = base_inputs(None, None);
        inputs.self_test = Some(&outcome);
        let checks = evaluate(&inputs);
        assert_eq!(status_of(&checks, "end_to_end"), CheckStatus::Warn);

        inputs.self_test = Some(&failed);
        let checks = evaluate(&inputs);
        let check = checks.iter().find(|c| c.id == "end_to_end").unwrap();
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.remedy.is_some());
    }

    #[test]
    fn identity_parses_and_tolerates_absence() {
        let identity = mint_identity(&json!({ "name": "M", "version": "cdk-mintd/1.0" }));
        assert_eq!(identity.name.as_deref(), Some("M"));
        assert_eq!(identity.version.as_deref(), Some("cdk-mintd/1.0"));
        let empty = mint_identity(&json!({}));
        assert!(empty.name.is_none());
    }
}
