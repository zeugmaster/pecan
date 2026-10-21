//! Operator web API for the branch payment backend.
//!
//! Auth: username + password against the users.json store. POST /api/login →
//! cookie session id, persisted in sessions.json so processor restarts do not
//! sign operators out. All other /api routes require a valid session whose
//! user still exists.
//!
//! The mint is external and operator-run: this API only reads its public
//! surfaces (checklist, identity, keysets, per-quote cross-checks) and writes
//! nothing but the processor's own attachment config — which applies live,
//! without a restart.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Json, Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio::sync::RwLock;
use tokio_stream::wrappers::BroadcastStream;

use crate::backend::BranchBackend;
use crate::checks::{self, Check, MintIdentity, SelfTestOutcome};
use crate::clients::{KeysetEntry, MintHttpClient};
use crate::config::{render_mint_snippet, AppConfig, ConfigStore, PASSWORD_MIN_LENGTH};
use crate::keysets::KeysetLog;
use crate::sessions::SessionStore;
use crate::state::{
    normalize_match_input, BranchState, MatchResult, Ticket, TicketKind, TicketStatus,
};
use crate::timefmt::{iso_date, iso_datetime};
use crate::users::{PublicUser, UserError, UserStore};

#[derive(Clone)]
pub struct WebState {
    pub branch: BranchState,
    pub backend: Arc<BranchBackend>,
    pub config: Arc<RwLock<AppConfig>>,
    pub config_store: ConfigStore,
    pub users: UserStore,
    pub sessions: SessionStore,
    /// Image/build version (CDK_BRANCH_PROCESSOR_VERSION), "dev" outside CI.
    pub version: Arc<String>,
    /// Where this process actually listens for the mint ("0.0.0.0:50051").
    pub grpc_bind: Arc<String>,
    /// Whether the gRPC endpoint serves TLS (CDK_BRANCH_PROCESSOR_TLS_DIR);
    /// decides which transport stanza the config snippet emits.
    pub grpc_tls: bool,
    /// The gRPC port as published on the host (compose may remap 50051);
    /// feeds the console's attachment prefill.
    pub published_grpc_port: u16,
    pub self_test: Arc<RwLock<Option<SelfTestOutcome>>>,
    pub self_test_running: Arc<AtomicBool>,
    /// When the mint's keysets were first seen and seen to stop issuing.
    pub keyset_log: KeysetLog,
}

impl WebState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        branch: BranchState,
        backend: Arc<BranchBackend>,
        config: Arc<RwLock<AppConfig>>,
        config_store: ConfigStore,
        users: UserStore,
        sessions: SessionStore,
        version: String,
        grpc_bind: String,
        grpc_tls: bool,
        published_grpc_port: u16,
        self_test: Arc<RwLock<Option<SelfTestOutcome>>>,
        self_test_running: Arc<AtomicBool>,
        keyset_log: KeysetLog,
    ) -> Self {
        Self {
            branch,
            backend,
            config,
            config_store,
            users,
            sessions,
            version: Arc::new(version),
            grpc_bind: Arc::new(grpc_bind),
            grpc_tls,
            published_grpc_port,
            self_test,
            self_test_running,
            keyset_log,
        }
    }
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(spa_page))
        .route("/teller", get(spa_page))
        .route("/login", get(spa_page))
        // Unauthenticated liveness probe for container healthchecks and the
        // installer's wait loop.
        .route("/healthz", get(healthz))
        .route("/api/app", get(api_app))
        .route("/api/login", post(api_login))
        .route("/api/logout", post(api_logout))
        .route("/api/quotes/match", post(api_match_quote))
        .route("/api/tickets", get(api_tickets))
        .route("/api/tickets.csv", get(api_tickets_csv))
        .route("/api/circulation", get(api_circulation))
        .route("/api/tickets/{id}/mark-paid", post(api_mark_paid))
        .route("/api/tickets/{id}/mark-failed", post(api_mark_failed))
        .route("/api/settings/attachment", post(api_set_attachment))
        .route("/api/mint/self-test", post(api_self_test))
        .route("/api/users", post(api_users_create))
        .route("/api/users/{username}", delete(api_users_delete))
        .route(
            "/api/users/{username}/password",
            post(api_users_reset_password),
        )
        .route("/api/me/password", post(api_me_password))
        // Server-sent events: the dashboard listens here and reloads when state changes.
        .route("/events", get(sse_events))
        .route("/assets/{*path}", get(spa_asset))
        .route("/favicon.svg", get(spa_favicon))
        // Self-hosted fonts (embedded in the binary so the UI works offline / behind
        // content blockers).
        .route("/static/inter.woff2", get(font_inter))
        .route("/static/jbm.woff2", get(font_jbm))
        .with_state(state)
}

#[derive(Serialize)]
struct HealthzBody {
    status: &'static str,
    version: String,
}

/// Liveness only, deliberately: an unattached processor is a normal
/// long-lived state and must not fail container health, and this is polled
/// every few seconds — no store reads, no mint probes, no auth. Subsystem
/// truth lives in the authenticated /api/app checklist. The version lets the
/// installer confirm which build is answering after an update.
async fn healthz(State(state): State<WebState>) -> Response {
    let mut resp = Json(HealthzBody {
        status: "ok",
        version: state.version.as_ref().clone(),
    })
    .into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    resp
}

async fn sse_events(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, Response> {
    if authenticated(&state, &headers).await.is_none() {
        return Err((StatusCode::UNAUTHORIZED, "unauthorized").into_response());
    }
    let rx = state.branch.subscribe_ui_changes();
    let stream = BroadcastStream::new(rx).filter_map(|r| async move {
        // Drop Lagged errors; treat every successful tick as one event.
        r.ok()
            .map(|()| Ok(SseEvent::default().event("change").data("1")))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

const INTER_WOFF2: &[u8] = include_bytes!("../assets/Inter-Variable.woff2");
const JBM_WOFF2: &[u8] = include_bytes!("../assets/JetBrainsMono.woff2");

async fn font_inter() -> impl IntoResponse {
    font_response(INTER_WOFF2)
}

async fn font_jbm() -> impl IntoResponse {
    font_response(JBM_WOFF2)
}

fn font_response(bytes: &'static [u8]) -> Response {
    use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
    (
        [
            (CONTENT_TYPE, "font/woff2"),
            (CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response()
}

// ---------------- SPA assets ----------------

async fn spa_page() -> Response {
    match read_spa_file("index.html").await {
        Ok(bytes) => bytes_response(bytes, "text/html; charset=utf-8"),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [("content-type", "text/html; charset=utf-8")],
            r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Branch</title></head>
  <body style="font-family: Inter, system-ui, sans-serif; margin: 40px; color: #0a0a0a">
    <h1>Operator UI assets are not built</h1>
    <p>Run <code>npm --prefix web run build</code> or set <code>CDK_BRANCH_PROCESSOR_WEB_DIST</code> to a built Vite dist directory.</p>
  </body>
</html>"#,
        )
            .into_response(),
    }
}

async fn spa_asset(AxumPath(path): AxumPath<String>) -> Response {
    if !is_safe_asset_path(&path) {
        return (StatusCode::BAD_REQUEST, "bad asset path").into_response();
    }
    let asset_path = format!("assets/{path}");
    match read_spa_file(&asset_path).await {
        Ok(bytes) => bytes_response(bytes, content_type_for(&asset_path)),
        Err(_) => (StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
}

async fn spa_favicon() -> Response {
    match read_spa_file("favicon.svg").await {
        Ok(bytes) => bytes_response(bytes, "image/svg+xml"),
        Err(_) => (StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
}

async fn read_spa_file(rel: &str) -> std::io::Result<Vec<u8>> {
    let rel = Path::new(rel);
    for dir in web_dist_candidates() {
        let path = dir.join(rel);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return tokio::fs::read(path).await;
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "web dist not found",
    ))
}

fn web_dist_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("CDK_BRANCH_PROCESSOR_WEB_DIST") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from("web/dist"));
    candidates.push(PathBuf::from("../web/dist"));
    candidates.push(PathBuf::from("/usr/local/share/pecan/web"));
    candidates
}

fn is_safe_asset_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

fn content_type_for(path: &str) -> &'static str {
    match Path::new(path).extension().and_then(|ext| ext.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn bytes_response(bytes: Vec<u8>, content_type: &'static str) -> Response {
    ([("content-type", content_type)], bytes).into_response()
}

// ---------------- JSON API ----------------

#[derive(Serialize)]
struct ApiError {
    error: String,
}

#[derive(Serialize)]
struct ApiMessage {
    message: &'static str,
}

#[derive(Serialize)]
struct LoginResponse {
    message: &'static str,
    must_change_password: bool,
    /// The login page renders the set-a-new-password step before any
    /// authenticated snapshot exists, so the rule ships with the response.
    password_min_length: usize,
}

fn api_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ApiError { error: msg.into() })).into_response()
}

/// The resolved identity behind a valid session cookie.
struct Authed {
    session_id: String,
    username: String,
    /// Still on the installer-provisioned password; must set their own first.
    must_change_password: bool,
}

/// Session-only check for the routes that must stay usable while a forced
/// password change is pending: the snapshot (the change screen renders from
/// it) and the password-change endpoint itself.
async fn require_api_session(state: &WebState, headers: &HeaderMap) -> Result<Authed, Response> {
    match authenticated(state, headers).await {
        Some(authed) => Ok(authed),
        None => Err(api_error(StatusCode::UNAUTHORIZED, "unauthorized")),
    }
}

/// Default auth for API handlers. A pending forced password change locks the
/// account down to `/api/app` and `/api/me/password` — every other route lands
/// here so a newly added handler is gated unless it opts out deliberately.
async fn require_api_auth(state: &WebState, headers: &HeaderMap) -> Result<Authed, Response> {
    let authed = require_api_session(state, headers).await?;
    if authed.must_change_password {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "set your own password before continuing",
        ));
    }
    Ok(authed)
}

#[derive(Serialize)]
struct ApiAppSnapshot {
    now: u64,
    session: ApiSessionInfo,
    users: Vec<PublicUser>,
    demo_password_active: bool,
    password_min_length: usize,
    version: String,
    setup: ApiSetup,
    attach_signals: ApiAttachSignals,
    checklist: Vec<Check>,
    self_test: Option<SelfTestOutcome>,
    /// The mint.toml fragment for the operator's cdk-mintd; None until the
    /// unit and advertised gRPC endpoint are configured.
    snippet: Option<String>,
    /// Read-only identity of the attached mint, from its /v1/info.
    mint_identity: Option<MintIdentity>,
    /// The configured unit's keysets at the mint (read-only).
    keysets: Vec<ApiKeyset>,
    keysets_error: Option<String>,
    summary: MintSummary,
    open_quotes: Vec<ApiOpenQuote>,
    recent_done: Vec<ApiTicket>,
    /// One-time migration notice: this install previously managed its mint.
    migrated_from_managed: bool,
}

#[derive(Serialize)]
struct ApiSessionInfo {
    username: String,
    /// Mirrors the login response so a reload mid-flow still lands on the
    /// forced-change screen instead of the console.
    must_change_password: bool,
}

#[derive(Serialize)]
struct ApiSetup {
    unit: String,
    unit_locked: bool,
    /// Whether the unit field is editable right now (not locked, and no real
    /// tickets reference it yet).
    unit_change_allowed: bool,
    method: String,
    mint_url: String,
    advertised_grpc: String,
    /// Where this process actually listens for the mint.
    grpc_bind: String,
    grpc_tls: bool,
    /// Host-published gRPC port, for the attachment prefill.
    published_grpc_port: u16,
    attached: bool,
    setup_complete: bool,
}

#[derive(Serialize)]
struct ApiAttachSignals {
    last_settings_at: Option<u64>,
    stream_attached_at: Option<u64>,
}

/// Full ticket view. Ships to the browser only for settled tickets and as the
/// response to a successful match — never for the open-quote list, whose ids
/// must stay off the operator's screen.
#[derive(Serialize)]
struct ApiTicket {
    id: String,
    short_id: String,
    /// The mint's quote id — what the customer's wallet displays.
    quote_id: Option<String>,
    kind: TicketKind,
    kind_label: &'static str,
    amount: u64,
    unit: String,
    status: TicketStatus,
    status_label: &'static str,
    created_at: u64,
    paid_at: Option<u64>,
    expires_at: Option<u64>,
    voided_at: Option<u64>,
    /// Settled or voided: when the ticket left the open list.
    closed_at: u64,
    /// The console user who settled or voided it.
    operator: Option<String>,
    description: Option<String>,
    notes: Option<String>,
}

/// Redacted row for the teller's open-quote list. `prefix` is the leading 13
/// characters of the quote id — the UUIDv7 timestamp section — so rows are
/// tellable apart while the random tail used for matching never renders.
#[derive(Serialize)]
struct ApiOpenQuote {
    prefix: String,
    kind: TicketKind,
    kind_label: &'static str,
    amount: u64,
    unit: String,
    status: TicketStatus,
    status_label: &'static str,
    created_at: u64,
    expires_at: Option<u64>,
}

impl ApiOpenQuote {
    fn from_ticket(ticket: &Ticket) -> Self {
        let prefix = ticket
            .quote_id
            .as_deref()
            .map(|quote_id| quote_id.chars().take(13).collect())
            .unwrap_or_else(|| short_id(&ticket.id));
        Self {
            prefix,
            kind: ticket.kind,
            kind_label: kind_label(ticket.kind),
            amount: ticket.amount,
            unit: ticket.unit.clone(),
            status: ticket.status,
            status_label: status_label(ticket.kind, ticket.status),
            created_at: ticket.created_at,
            expires_at: ticket.expires_at,
        }
    }
}

/// A keyset of the configured unit as the mint lists it, plus what this
/// processor observed of its lifetime (see keysets.rs).
#[derive(Serialize)]
struct ApiKeyset {
    #[serde(flatten)]
    keyset: KeysetEntry,
    first_seen_at: Option<u64>,
    /// False when the keyset predates this processor's first look at the
    /// mint: it started issuing some time before `first_seen_at`.
    start_observed: bool,
    deactivated_at: Option<u64>,
}

fn mint_client_for(config: &AppConfig) -> Option<MintHttpClient> {
    if config.mint_url.is_empty() {
        None
    } else {
        Some(MintHttpClient::new(config.mint_url.clone()))
    }
}

async fn api_app(State(state): State<WebState>, headers: HeaderMap) -> Response {
    // Session-only: the forced-password-change screen renders from this
    // snapshot, so it must stay readable while the change is pending.
    let authed = match require_api_session(&state, &headers).await {
        Ok(authed) => authed,
        Err(r) => return r,
    };
    let config = state.config.read().await.clone();
    let now = unix_now();

    // Self-test probes are voided instantly and filtered from every
    // operator-facing list — they are plumbing, not activity.
    let mut tickets = state.branch.list_all().await;
    tickets.retain(|ticket| !checks::is_self_test_ticket(ticket));
    tickets.sort_by_key(|ticket| std::cmp::Reverse(ticket.created_at));
    let has_real_tickets = !tickets.is_empty();
    let unit_tickets: Vec<Ticket> = tickets
        .iter()
        .filter(|ticket| ticket.unit == config.unit)
        .cloned()
        .collect();
    let summary = MintSummary::from_tickets(&unit_tickets);
    let open_quotes = tickets
        .iter()
        .filter(|t| t.status.is_active())
        .map(ApiOpenQuote::from_ticket)
        .collect();
    let mut done: Vec<&Ticket> = tickets.iter().filter(|t| !t.status.is_active()).collect();
    sort_newest_closed_first(&mut done);
    let recent_done = done
        .into_iter()
        .take(50)
        .map(ApiTicket::from_ticket)
        .collect();

    // Probe the attached mint's public surfaces (short timeouts; see clients.rs).
    let mint = mint_client_for(&config);
    let info_result = match &mint {
        Some(client) => Some(client.get_info().await),
        None => None,
    };
    let keysets_result = match &mint {
        Some(client) => Some(client.list_keysets().await),
        None => None,
    };

    let self_test = state.self_test.read().await.clone();
    let checklist = checks::evaluate(&checks::ChecklistInputs {
        now,
        unit: &config.unit,
        method: &config.method,
        mint_url: &config.mint_url,
        info: info_result.as_ref(),
        keysets: keysets_result.as_ref(),
        last_settings_at: state.backend.last_settings_at(),
        stream_attached_at: state.backend.stream_attached_at(),
        self_test: self_test.as_ref(),
    });
    let mint_identity = match &info_result {
        Some(Ok(info)) => Some(checks::mint_identity(info)),
        _ => None,
    };
    if let Some(Ok(items)) = &keysets_result {
        state.keyset_log.observe(items, now).await;
    }
    let sightings = state.keyset_log.sightings().await;
    let (keysets, keysets_error) = match keysets_result {
        Some(Ok(items)) => (
            items
                .into_iter()
                .filter(|keyset| keyset.unit == config.unit)
                .map(|keyset| {
                    let sighting = sightings.get(&keyset.id);
                    ApiKeyset {
                        first_seen_at: sighting.map(|s| s.first_seen_at),
                        start_observed: sighting.is_some_and(|s| s.start_observed),
                        deactivated_at: sighting.and_then(|s| s.deactivated_at),
                        keyset,
                    }
                })
                .collect(),
            None,
        ),
        Some(Err(e)) => (Vec::new(), Some(format!("{e:#}"))),
        None => (Vec::new(), None),
    };

    Json(ApiAppSnapshot {
        now,
        session: ApiSessionInfo {
            username: authed.username,
            must_change_password: authed.must_change_password,
        },
        users: state.users.list().await,
        demo_password_active: state.users.demo_password_active().await,
        password_min_length: PASSWORD_MIN_LENGTH,
        version: state.version.as_ref().clone(),
        setup: ApiSetup {
            unit: config.unit.clone(),
            unit_locked: config.unit_locked,
            unit_change_allowed: !config.unit_locked && !has_real_tickets,
            method: config.method.clone(),
            mint_url: config.mint_url.clone(),
            advertised_grpc: config.advertised_grpc.clone(),
            grpc_bind: state.grpc_bind.as_ref().clone(),
            grpc_tls: state.grpc_tls,
            published_grpc_port: state.published_grpc_port,
            attached: config.is_attached(),
            setup_complete: config.setup_complete(),
        },
        attach_signals: ApiAttachSignals {
            last_settings_at: state.backend.last_settings_at(),
            stream_attached_at: state.backend.stream_attached_at(),
        },
        checklist,
        self_test,
        snippet: render_mint_snippet(&config, state.grpc_tls),
        mint_identity,
        keysets,
        keysets_error,
        summary,
        open_quotes,
        recent_done,
        migrated_from_managed: config.migrated_from_managed,
    })
    .into_response()
}

impl ApiTicket {
    fn from_ticket(ticket: &Ticket) -> Self {
        Self {
            id: ticket.id.clone(),
            short_id: short_id(&ticket.id),
            quote_id: ticket.quote_id.clone(),
            kind: ticket.kind,
            kind_label: kind_label(ticket.kind),
            amount: ticket.amount,
            unit: ticket.unit.clone(),
            status: ticket.status,
            status_label: status_label(ticket.kind, ticket.status),
            created_at: ticket.created_at,
            paid_at: ticket.paid_at,
            expires_at: ticket.expires_at,
            voided_at: ticket.voided_at,
            closed_at: ticket.closed_at(),
            operator: ticket.operator.clone(),
            description: ticket.description.clone(),
            notes: ticket.notes.clone(),
        }
    }
}

/// Settled history order: most recently settled or voided first.
fn sort_newest_closed_first(tickets: &mut [&Ticket]) {
    tickets.sort_by(|a, b| {
        b.closed_at()
            .cmp(&a.closed_at())
            .then_with(|| b.id.cmp(&a.id))
    });
}

// ---------------- settled history ----------------

const HISTORY_DEFAULT_LIMIT: usize = 25;
const HISTORY_MAX_LIMIT: usize = 200;

/// Filters for the settled-activity history (`/api/tickets` and its CSV).
/// Only closed tickets are ever listed: open quote ids stay off the screen.
#[derive(Deserialize, Default)]
struct HistoryQuery {
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
    kind: Option<TicketKind>,
    status: Option<TicketStatus>,
    /// Ticket or quote id fragment, note text, or operator.
    #[serde(default)]
    q: String,
    /// Closed at or after (unix seconds).
    from: Option<u64>,
    /// Closed before (unix seconds).
    to: Option<u64>,
}

impl HistoryQuery {
    fn matches(&self, ticket: &Ticket) -> bool {
        if ticket.status.is_active() || checks::is_self_test_ticket(ticket) {
            return false;
        }
        if self.kind.is_some_and(|kind| kind != ticket.kind)
            || self.status.is_some_and(|status| status != ticket.status)
        {
            return false;
        }
        let closed = ticket.closed_at();
        if self.from.is_some_and(|from| closed < from) || self.to.is_some_and(|to| closed >= to) {
            return false;
        }
        let needle = self.q.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        let hex_needle: String = needle.chars().filter(|c| *c != '-').collect();
        let contains = |text: Option<&str>| {
            text.is_some_and(|text| text.to_lowercase().contains(needle.as_str()))
        };
        contains(Some(&ticket.id))
            || contains(ticket.notes.as_deref())
            || contains(ticket.operator.as_deref())
            || (!hex_needle.is_empty()
                && ticket.quote_id.as_deref().is_some_and(|quote_id| {
                    quote_id
                        .to_lowercase()
                        .replace('-', "")
                        .contains(hex_needle.as_str())
                }))
    }
}

async fn settled_history(state: &WebState, query: &HistoryQuery) -> Vec<Ticket> {
    let tickets = state.branch.list_all().await;
    let mut matched: Vec<&Ticket> = tickets.iter().filter(|t| query.matches(t)).collect();
    sort_newest_closed_first(&mut matched);
    matched.into_iter().cloned().collect()
}

#[derive(Serialize)]
struct ApiHistoryPage {
    tickets: Vec<ApiTicket>,
    /// Rows matching the filters, across all pages.
    total: usize,
    offset: usize,
    limit: usize,
    /// Paid totals for the configured unit over every matching row.
    summary: MintSummary,
}

async fn api_tickets(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Response {
    if let Err(r) = require_api_auth(&state, &headers).await {
        return r;
    }
    let unit = state.config.read().await.unit.clone();
    let matched = settled_history(&state, &query).await;
    let limit = query
        .limit
        .unwrap_or(HISTORY_DEFAULT_LIMIT)
        .clamp(1, HISTORY_MAX_LIMIT);
    let unit_rows: Vec<Ticket> = matched.iter().filter(|t| t.unit == unit).cloned().collect();
    Json(ApiHistoryPage {
        tickets: matched
            .iter()
            .skip(query.offset)
            .take(limit)
            .map(ApiTicket::from_ticket)
            .collect(),
        total: matched.len(),
        offset: query.offset,
        limit,
        summary: MintSummary::from_tickets(&unit_rows),
    })
    .into_response()
}

/// The same filtered history as a spreadsheet-ready CSV (UTC timestamps).
async fn api_tickets_csv(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Response {
    if let Err(r) = require_api_auth(&state, &headers).await {
        return r;
    }
    let unit = state.config.read().await.unit.clone();
    let rows = settled_history(&state, &query).await;
    let filename = if unit.is_empty() {
        format!("pecan-activity-{}.csv", iso_date(unix_now()))
    } else {
        format!("pecan-activity-{unit}-{}.csv", iso_date(unix_now()))
    };
    (
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        history_csv(&rows),
    )
        .into_response()
}

fn history_csv(rows: &[Ticket]) -> String {
    let mut out = String::from(
        "ticket,quote_id,kind,amount,unit,status,created_at,closed_at,operator,notes\n",
    );
    for ticket in rows {
        let fields = [
            ticket.id.clone(),
            ticket.quote_id.clone().unwrap_or_default(),
            match ticket.kind {
                TicketKind::Incoming => "deposit",
                TicketKind::Outgoing => "payout",
            }
            .to_string(),
            ticket.amount.to_string(),
            ticket.unit.clone(),
            match ticket.status {
                TicketStatus::Paid => "paid",
                _ => "failed",
            }
            .to_string(),
            iso_datetime(ticket.created_at),
            iso_datetime(ticket.closed_at()),
            ticket.operator.clone().unwrap_or_default(),
            ticket.notes.clone().unwrap_or_default(),
        ];
        let line: Vec<String> = fields.iter().map(|field| csv_field(field)).collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }
    out
}

/// Quote a CSV field when needed, and defuse spreadsheet formulas in free
/// text (operator notes) by prefixing an apostrophe.
fn csv_field(raw: &str) -> String {
    let defused = if raw.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{raw}")
    } else {
        raw.to_string()
    };
    if defused.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", defused.replace('"', "\"\""))
    } else {
        defused
    }
}

// ---------------- circulation ----------------

/// Upper bound on returned days (ten years), whatever the range asked for.
const CIRCULATION_MAX_DAYS: i64 = 3_660;

#[derive(Deserialize)]
struct CirculationQuery {
    /// Trailing days to return, today included; omitted = since the first
    /// settlement.
    days: Option<u32>,
    /// The viewer's offset from UTC in minutes (east positive), so days break
    /// at the viewer's midnight.
    #[serde(default)]
    utc_offset: i32,
}

/// One calendar day of settled activity for the configured unit.
#[derive(Debug, Serialize)]
struct CirculationDay {
    /// Unix seconds of the day's (viewer-local) midnight.
    start: i64,
    deposits: u64,
    deposit_count: usize,
    payouts: u64,
    payout_count: usize,
    /// Net settled (deposits minus payouts since the beginning) at day end.
    circulation: i128,
}

async fn api_circulation(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<CirculationQuery>,
) -> Response {
    if let Err(r) = require_api_auth(&state, &headers).await {
        return r;
    }
    let unit = state.config.read().await.unit.clone();
    let tickets: Vec<Ticket> = state
        .branch
        .list_all()
        .await
        .into_iter()
        .filter(|ticket| ticket.unit == unit)
        .collect();
    let utc_offset_secs = i64::from(query.utc_offset.clamp(-840, 840)) * 60;
    Json(circulation_days(
        &tickets,
        query.days,
        utc_offset_secs,
        unix_now(),
    ))
    .into_response()
}

/// Daily buckets from the range start through today, every day present (so
/// charts space time evenly), with the running total carried in from before
/// the range. Empty until the first settlement.
fn circulation_days(
    tickets: &[Ticket],
    days: Option<u32>,
    utc_offset_secs: i64,
    now: u64,
) -> Vec<CirculationDay> {
    let day_of = |ts: u64| (ts as i64 + utc_offset_secs).div_euclid(86_400);
    let paid: Vec<&Ticket> = tickets
        .iter()
        .filter(|ticket| ticket.status == TicketStatus::Paid)
        .collect();
    let Some(first_paid) = paid.iter().map(|ticket| day_of(ticket.closed_at())).min() else {
        return Vec::new();
    };
    let today = day_of(now);
    let first = match days {
        Some(days) if days > 0 => today - i64::from(days) + 1,
        _ => first_paid,
    }
    .clamp(today - CIRCULATION_MAX_DAYS + 1, today);

    let mut out: Vec<CirculationDay> = (first..=today)
        .map(|day| CirculationDay {
            start: day * 86_400 - utc_offset_secs,
            deposits: 0,
            deposit_count: 0,
            payouts: 0,
            payout_count: 0,
            circulation: 0,
        })
        .collect();
    let mut carried = 0i128;
    for ticket in paid {
        // A settlement stamped after `now` (clock skew) counts as today.
        let day = day_of(ticket.closed_at()).min(today);
        if day < first {
            carried += match ticket.kind {
                TicketKind::Incoming => ticket.amount as i128,
                TicketKind::Outgoing => -(ticket.amount as i128),
            };
            continue;
        }
        let bucket = &mut out[(day - first) as usize];
        match ticket.kind {
            TicketKind::Incoming => {
                bucket.deposits = bucket.deposits.saturating_add(ticket.amount);
                bucket.deposit_count += 1;
            }
            TicketKind::Outgoing => {
                bucket.payouts = bucket.payouts.saturating_add(ticket.amount);
                bucket.payout_count += 1;
            }
        }
    }
    let mut running = carried;
    for bucket in &mut out {
        running += bucket.deposits as i128 - bucket.payouts as i128;
        bucket.circulation = running;
    }
    out
}

async fn api_login(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(form): Json<LoginForm>,
) -> Response {
    let username = form.username.trim().to_ascii_lowercase();
    if !state.users.verify(&username, &form.password).await {
        return api_error(StatusCode::UNAUTHORIZED, "incorrect username or password");
    }
    let sid = uuid::Uuid::new_v4().to_string();
    state.sessions.insert(&sid, &username).await;
    let mut resp = Json(LoginResponse {
        message: "signed_in",
        // Tells the login page to go straight into the set-a-new-password
        // step instead of the console.
        must_change_password: state.users.must_change_password(&username).await,
        password_min_length: PASSWORD_MIN_LENGTH,
    })
    .into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&sid, request_is_https(&headers))
            .parse()
            .unwrap(),
    );
    resp
}

async fn api_logout(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Some(c) = cookie_value(&headers) {
        state.sessions.remove(&c).await;
    }
    let mut resp = Json(ApiMessage {
        message: "signed_out",
    })
    .into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        clear_session_cookie(request_is_https(&headers))
            .parse()
            .unwrap(),
    );
    resp
}

#[derive(Deserialize)]
struct MatchQuoteForm {
    code: String,
}

/// Resolve teller input (last 6+ characters typed from the customer's wallet,
/// or the full quote id from a scanner) to the one open quote it identifies.
/// The full ticket is revealed only here — the open-quote list stays redacted
/// so the code must come from the customer.
async fn api_match_quote(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(form): Json<MatchQuoteForm>,
) -> Response {
    if let Err(r) = require_api_auth(&state, &headers).await {
        return r;
    }
    let query = match normalize_match_input(&form.code) {
        Ok(query) => query,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, e),
    };
    match state.branch.match_open(&query).await {
        MatchResult::Ambiguous(n) => api_error(
            StatusCode::CONFLICT,
            format!("{n} open quotes end with this code — type more characters or scan the full id"),
        ),
        MatchResult::None {
            inactive_match: true,
        } => api_error(
            StatusCode::NOT_FOUND,
            "this code matches a quote that is no longer open (settled, voided, or expired)",
        ),
        MatchResult::None {
            inactive_match: false,
        } => api_error(StatusCode::NOT_FOUND, "no open quote matches this code"),
        MatchResult::Unique(ticket) => {
            if ticket.expired(unix_now()) {
                return api_error(
                    StatusCode::NOT_FOUND,
                    "this quote has expired; ask the customer to create a fresh one",
                );
            }
            if let Err(r) = verify_with_mint(&state, &ticket).await {
                return r;
            }
            Json(ApiTicket::from_ticket(&ticket)).into_response()
        }
    }
}

/// Cross-check a matched ticket against the mint's own record before the
/// operator sees a confirm card. Catches orphaned tickets (the mint died
/// before committing the quote), quotes already settled at the mint, and a
/// quote that is not NUT-20-locked. Refuses when the mint is unreachable:
/// confirming cash movements on stale knowledge is worse than asking the
/// customer to wait a moment.
async fn verify_with_mint(state: &WebState, ticket: &Ticket) -> Result<(), Response> {
    let Some(quote_id) = ticket.quote_id.as_deref() else {
        return Err(api_error(
            StatusCode::CONFLICT,
            "this ticket has no mint quote id and cannot be settled",
        ));
    };
    let config = state.config.read().await.clone();
    let Some(mint) = mint_client_for(&config) else {
        return Err(api_error(
            StatusCode::BAD_GATEWAY,
            "no mint is attached — set the mint URL in the Mint tab before settling",
        ));
    };
    let method = config.method.as_str();
    match ticket.kind {
        TicketKind::Incoming => match mint.get_mint_quote(method, quote_id).await {
            Err(e) => Err(api_error(
                StatusCode::BAD_GATEWAY,
                format!("cannot verify the quote with the mint right now: {e}"),
            )),
            Ok(None) => Err(api_error(
                StatusCode::CONFLICT,
                "the mint does not know this quote — do not accept cash for it",
            )),
            Ok(Some(quote)) => {
                if quote.amount_paid > 0 || quote.amount_issued > 0 {
                    Err(api_error(
                        StatusCode::CONFLICT,
                        "this quote is already settled at the mint",
                    ))
                } else if quote.request != ticket.id
                    || quote.amount.is_some_and(|amount| amount != ticket.amount)
                    || quote.unit.as_deref().is_some_and(|unit| unit != ticket.unit)
                {
                    Err(api_error(
                        StatusCode::CONFLICT,
                        "the mint's record disagrees with this quote; do not settle it",
                    ))
                } else if quote.pubkey.as_deref().unwrap_or("").is_empty() {
                    // Cannot happen for quotes created through this backend
                    // (creation refuses unlocked quotes) — seeing it means the
                    // quote was created against a different processor.
                    Err(api_error(
                        StatusCode::CONFLICT,
                        "this quote is not locked to a wallet key (NUT-20); do not accept cash for it",
                    ))
                } else {
                    Ok(())
                }
            }
        },
        TicketKind::Outgoing => match mint.get_melt_quote(method, quote_id).await {
            Err(e) => Err(api_error(
                StatusCode::BAD_GATEWAY,
                format!("cannot verify the quote with the mint right now: {e}"),
            )),
            Ok(None) => Err(api_error(
                StatusCode::CONFLICT,
                "the mint does not know this melt quote — do not pay out",
            )),
            Ok(Some(quote)) => {
                if quote.state.eq_ignore_ascii_case("paid") {
                    Err(api_error(
                        StatusCode::CONFLICT,
                        "this melt is already settled at the mint",
                    ))
                } else if quote.amount != ticket.amount
                    || quote.unit.as_deref().is_some_and(|unit| unit != ticket.unit)
                {
                    Err(api_error(
                        StatusCode::CONFLICT,
                        "the mint's record disagrees with this quote; do not settle it",
                    ))
                } else {
                    Ok(())
                }
            }
        },
    }
}

async fn api_mark_paid(
    State(state): State<WebState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(form): Json<NotesForm>,
) -> Response {
    let authed = match require_api_auth(&state, &headers).await {
        Ok(authed) => authed,
        Err(r) => return r,
    };
    match mark_paid_inner(&state, &id, form.notes, authed.username).await {
        Ok(ticket) => Json(ApiTicket::from_ticket(&ticket)).into_response(),
        Err(e) => api_error(StatusCode::BAD_REQUEST, e),
    }
}

async fn api_mark_failed(
    State(state): State<WebState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(form): Json<NotesForm>,
) -> Response {
    let authed = match require_api_auth(&state, &headers).await {
        Ok(authed) => authed,
        Err(r) => return r,
    };
    match mark_failed_inner(&state, &id, form.notes, authed.username).await {
        Ok(ticket) => Json(ApiTicket::from_ticket(&ticket)).into_response(),
        Err(e) => api_error(StatusCode::BAD_REQUEST, e),
    }
}

// ---------------- attachment setup ----------------

#[derive(Deserialize)]
struct AttachmentForm {
    /// Omitted or empty = keep the current unit.
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    mint_url: String,
    #[serde(default)]
    advertised_grpc: String,
}

/// Apply the console's setup/attachment form. Everything applies live — the
/// gRPC backend picks the unit up immediately, no restart. The attached mint
/// reads our settings at ITS next start, which the checklist reminds the
/// operator about.
async fn api_set_attachment(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(form): Json<AttachmentForm>,
) -> Response {
    if let Err(r) = require_api_auth(&state, &headers).await {
        return r;
    }
    let mut config = state.config.read().await.clone();
    let before_unit = config.unit.clone();
    let before_url = config.mint_url.clone();

    let requested_unit = form
        .unit
        .as_deref()
        .map(str::trim)
        .filter(|unit| !unit.is_empty());
    if let Some(unit) = requested_unit {
        let normalized = unit.to_ascii_lowercase();
        if normalized != config.unit {
            if config.unit_locked {
                return api_error(
                    StatusCode::CONFLICT,
                    format!(
                        "the unit is locked: ecash and quotes reference {} — see the \
                         operations guide before changing it",
                        config.unit
                    ),
                );
            }
            let has_real_tickets = state
                .branch
                .list_all()
                .await
                .iter()
                .any(|ticket| !checks::is_self_test_ticket(ticket));
            if has_real_tickets {
                return api_error(
                    StatusCode::CONFLICT,
                    "tickets already reference the current unit; the unit can only be \
                     changed on an unused install",
                );
            }
        }
    }
    if let Err(e) = config.set_attachment(requested_unit, &form.mint_url, &form.advertised_grpc) {
        return api_error(StatusCode::BAD_REQUEST, e.to_string());
    }
    if let Err(e) = state.config_store.save(&config).await {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("save configuration: {e:#}"),
        );
    }

    // Live-apply the unit to the gRPC backend.
    let unit = if config.unit.is_empty() {
        None
    } else {
        match config.unit.parse() {
            Ok(unit) => Some(unit),
            Err(e) => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    format!("unit {}: {e}", config.unit),
                )
            }
        }
    };
    state.backend.set_unit(unit);

    // A different unit or mint invalidates the previous end-to-end result.
    if config.unit != before_unit || config.mint_url != before_url {
        *state.self_test.write().await = None;
    }
    *state.config.write().await = config;
    state.branch.notify_ui_change();
    Json(ApiMessage { message: "saved" }).into_response()
}

// ---------------- self-test ----------------

async fn api_self_test(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Err(r) = require_api_auth(&state, &headers).await {
        return r;
    }
    let config = state.config.read().await.clone();
    if !config.setup_complete() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "complete setup first — the self-test needs a unit and a mint URL",
        );
    }
    if state.self_test_running.swap(true, Ordering::SeqCst) {
        return api_error(StatusCode::CONFLICT, "a self-test is already running");
    }
    let outcome = checks::run_self_test(
        &MintHttpClient::new(config.mint_url.clone()),
        &state.branch,
        &config.method,
        &config.unit,
    )
    .await;
    state.self_test_running.store(false, Ordering::SeqCst);

    if outcome.ok {
        // The unit has now demonstrably been exercised against this mint.
        let updated = {
            let mut config = state.config.write().await;
            if !config.unit_locked {
                config.lock_unit();
                Some(config.clone())
            } else {
                None
            }
        };
        if let Some(updated) = updated {
            if let Err(e) = state.config_store.save(&updated).await {
                tracing::warn!("could not persist unit lock: {e:#}");
            }
        }
    }
    *state.self_test.write().await = Some(outcome.clone());
    state.branch.notify_ui_change();
    Json(outcome).into_response()
}

// ---------------- users ----------------

#[derive(Deserialize)]
struct CreateUserForm {
    username: String,
    password: String,
    password_confirm: String,
}

#[derive(Deserialize)]
struct PasswordChangeForm {
    current_password: String,
    password: String,
    password_confirm: String,
}

#[derive(Deserialize)]
struct PasswordResetForm {
    password: String,
    password_confirm: String,
}

fn user_error_response(e: anyhow::Error) -> Response {
    let status = match e.downcast_ref::<UserError>() {
        Some(UserError::Invalid(_)) => StatusCode::BAD_REQUEST,
        Some(UserError::Duplicate(_)) => StatusCode::CONFLICT,
        Some(UserError::Unknown(_)) => StatusCode::NOT_FOUND,
        Some(UserError::DeleteSelf) | Some(UserError::DeleteLast) => StatusCode::CONFLICT,
        None => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, e.to_string())
}

async fn api_users_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(form): Json<CreateUserForm>,
) -> Response {
    if let Err(r) = require_api_auth(&state, &headers).await {
        return r;
    }
    match state
        .users
        .create(&form.username, &form.password, &form.password_confirm)
        .await
    {
        Ok(user) => {
            state.branch.notify_ui_change();
            Json(user).into_response()
        }
        Err(e) => user_error_response(e),
    }
}

async fn api_users_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    AxumPath(username): AxumPath<String>,
) -> Response {
    let authed = match require_api_auth(&state, &headers).await {
        Ok(authed) => authed,
        Err(r) => return r,
    };
    let username = username.trim().to_ascii_lowercase();
    match state.users.delete(&username, &authed.username).await {
        Ok(()) => {
            state.sessions.remove_for_user(&username, None).await;
            state.branch.notify_ui_change();
            Json(ApiMessage { message: "deleted" }).into_response()
        }
        Err(e) => user_error_response(e),
    }
}

async fn api_me_password(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(form): Json<PasswordChangeForm>,
) -> Response {
    // Session-only: this is the one mutation a forced password change allows —
    // it is how the account gets out of that state.
    let authed = match require_api_session(&state, &headers).await {
        Ok(authed) => authed,
        Err(r) => return r,
    };
    if !state
        .users
        .verify(&authed.username, &form.current_password)
        .await
    {
        return api_error(StatusCode::FORBIDDEN, "current password is incorrect");
    }
    match state
        .users
        .set_password(&authed.username, &form.password, &form.password_confirm)
        .await
    {
        Ok(()) => {
            // Other sessions of this user die; the current one survives.
            state
                .sessions
                .remove_for_user(&authed.username, Some(&authed.session_id))
                .await;
            state.branch.notify_ui_change();
            Json(ApiMessage {
                message: "password_changed",
            })
            .into_response()
        }
        Err(e) => user_error_response(e),
    }
}

async fn api_users_reset_password(
    State(state): State<WebState>,
    headers: HeaderMap,
    AxumPath(username): AxumPath<String>,
    Json(form): Json<PasswordResetForm>,
) -> Response {
    let authed = match require_api_auth(&state, &headers).await {
        Ok(authed) => authed,
        Err(r) => return r,
    };
    let username = username.trim().to_ascii_lowercase();
    if username == authed.username {
        // Self-changes must go through /api/me/password so the
        // current-password check cannot be sidestepped.
        return api_error(
            StatusCode::FORBIDDEN,
            "use /api/me/password to change your own password",
        );
    }
    match state
        .users
        .set_password(&username, &form.password, &form.password_confirm)
        .await
    {
        Ok(()) => {
            state.sessions.remove_for_user(&username, None).await;
            state.branch.notify_ui_change();
            Json(ApiMessage {
                message: "password_changed",
            })
            .into_response()
        }
        Err(e) => user_error_response(e),
    }
}

// ---------------- session ----------------

const COOKIE_NAME: &str = "branch_session";

/// True when the request reached us over HTTPS through the reverse proxy.
/// X-Forwarded-Proto is trusted as-is: the bundled Caddy always sets it, and
/// a client faking the header on a direct HTTP connection only marks its own
/// cookie Secure — its browser then refuses to store it, harming nobody else.
fn request_is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
}

/// SameSite=Lax (Strict would drop the cookie on top-level navigations and
/// bounce operators to /login); no `__Host-` prefix, which would forbid the
/// cookie entirely on plain-HTTP dev/LAN deployments.
fn session_cookie(session_id: &str, secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!("{COOKIE_NAME}={session_id}; Path=/; HttpOnly; SameSite=Lax{secure_attr}")
}

/// Cookie identity is name+path, so clearing works regardless of the Secure
/// attribute; carrying it keeps strict-secure-cookie browsers happy. The
/// server-side session removal is the real logout.
fn clear_session_cookie(secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!("{COOKIE_NAME}=deleted; Path=/; Max-Age=0{secure_attr}")
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for c in cookie_header.split(';') {
        let c = c.trim();
        if let Some(rest) = c.strip_prefix(&format!("{COOKIE_NAME}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

async fn authenticated(state: &WebState, headers: &HeaderMap) -> Option<Authed> {
    let session_id = cookie_value(headers)?;
    let username = state.sessions.username_for(&session_id).await?;
    // Defense in depth: a persisted session of a since-deleted (or
    // corruption-reset) user must not authenticate.
    if !state.users.contains(&username).await {
        state.sessions.remove(&session_id).await;
        return None;
    }
    let must_change_password = state.users.must_change_password(&username).await;
    Some(Authed {
        session_id,
        username,
        must_change_password,
    })
}

#[derive(Serialize)]
struct MintSummary {
    mint_count: usize,
    melt_count: usize,
    minted_amount: u64,
    melted_amount: u64,
    net_issued: i128,
}

impl MintSummary {
    fn from_tickets(tickets: &[Ticket]) -> Self {
        let mut summary = Self {
            mint_count: 0,
            melt_count: 0,
            minted_amount: 0,
            melted_amount: 0,
            net_issued: 0,
        };
        for ticket in tickets {
            if ticket.status != TicketStatus::Paid {
                continue;
            }
            match ticket.kind {
                TicketKind::Incoming => {
                    summary.mint_count += 1;
                    summary.minted_amount = summary.minted_amount.saturating_add(ticket.amount);
                    summary.net_issued += ticket.amount as i128;
                }
                TicketKind::Outgoing => {
                    summary.melt_count += 1;
                    summary.melted_amount = summary.melted_amount.saturating_add(ticket.amount);
                    summary.net_issued -= ticket.amount as i128;
                }
            }
        }
        summary
    }
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct NotesForm {
    #[serde(default)]
    notes: String,
}

fn clean_notes(notes: String) -> Option<String> {
    let notes = notes.trim().to_string();
    if notes.is_empty() {
        None
    } else {
        Some(notes)
    }
}

async fn mark_paid_inner(
    state: &WebState,
    id: &str,
    notes: String,
    operator: String,
) -> Result<Ticket, String> {
    state
        .branch
        .mark_paid(id, clean_notes(notes), Some(operator))
        .await
        .map_err(|e| format!("mark_paid: {e}"))
}

async fn mark_failed_inner(
    state: &WebState,
    id: &str,
    notes: String,
    operator: String,
) -> Result<Ticket, String> {
    state
        .branch
        .mark_failed(id, clean_notes(notes), Some(operator))
        .await
        .map_err(|e| format!("mark_failed: {e}"))
}

// ---------------- helpers ----------------

fn short_id(id: &str) -> String {
    // "MINT-5a6c5a9e-..." → "MINT-5a6c5a9e"
    match id
        .find('-')
        .and_then(|p1| id[p1 + 1..].find('-').map(|p2| p1 + 1 + p2))
    {
        Some(end) => id[..end].to_string(),
        None => id.to_string(),
    }
}

fn kind_label(k: TicketKind) -> &'static str {
    match k {
        TicketKind::Incoming => "Incoming",
        TicketKind::Outgoing => "Outgoing",
    }
}

fn status_label(kind: TicketKind, status: TicketStatus) -> &'static str {
    match (kind, status) {
        (_, TicketStatus::Waiting) => "Awaiting wallet",
        (TicketKind::Incoming, TicketStatus::Pending) => "Awaiting cash",
        (TicketKind::Outgoing, TicketStatus::Pending) => "Ready to pay out",
        (_, TicketStatus::Paid) => "Paid",
        (_, TicketStatus::Failed) => "Failed",
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

    fn headers_with_proto(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", value.parse().unwrap());
        headers
    }

    fn closed(
        quote_id: &str,
        kind: TicketKind,
        amount: u64,
        status: TicketStatus,
        at: u64,
    ) -> Ticket {
        let mut ticket = match kind {
            TicketKind::Incoming => {
                Ticket::new_incoming(quote_id.into(), amount, "ora".into(), None, None)
            }
            TicketKind::Outgoing => {
                Ticket::new_outgoing(quote_id.into(), amount, "ora".into(), None, None)
            }
        };
        ticket.status = status;
        match status {
            TicketStatus::Paid => ticket.paid_at = Some(at),
            _ => ticket.voided_at = Some(at),
        }
        ticket
    }

    #[test]
    fn history_filters_closed_tickets_only() {
        let mut paid = closed(
            "0198c0ef-3f11-7abc-9def-aaaaaa9ec0f4",
            TicketKind::Incoming,
            500,
            TicketStatus::Paid,
            1_000,
        );
        paid.operator = Some("teller".into());
        let mut voided = closed(
            "0198c0ef-3f11-7abc-9def-bbbbbb111111",
            TicketKind::Outgoing,
            70,
            TicketStatus::Failed,
            2_000,
        );
        voided.notes = Some("Customer left".into());
        let open = Ticket::new_incoming(
            "0198c0ef-3f11-7abc-9def-cccccc222222".into(),
            9,
            "ora".into(),
            None,
            None,
        );

        let all = HistoryQuery::default();
        assert!(all.matches(&paid) && all.matches(&voided));
        assert!(
            !all.matches(&open),
            "open quotes never appear in the history"
        );

        let by_tail = HistoryQuery {
            q: "9EC0F4".into(),
            ..Default::default()
        };
        assert!(by_tail.matches(&paid) && !by_tail.matches(&voided));
        let by_hyphenated = HistoryQuery {
            q: "9def-aaaaaa".into(),
            ..Default::default()
        };
        assert!(by_hyphenated.matches(&paid));
        let by_note = HistoryQuery {
            q: "customer".into(),
            ..Default::default()
        };
        assert!(by_note.matches(&voided) && !by_note.matches(&paid));
        let by_operator = HistoryQuery {
            q: "teller".into(),
            ..Default::default()
        };
        assert!(by_operator.matches(&paid) && !by_operator.matches(&voided));

        let payouts = HistoryQuery {
            kind: Some(TicketKind::Outgoing),
            ..Default::default()
        };
        assert!(payouts.matches(&voided) && !payouts.matches(&paid));
        let window = HistoryQuery {
            from: Some(1_000),
            to: Some(2_000),
            ..Default::default()
        };
        assert!(window.matches(&paid) && !window.matches(&voided));

        let mut rows = vec![&paid, &voided];
        sort_newest_closed_first(&mut rows);
        assert_eq!(rows[0].id, voided.id);
    }

    #[test]
    fn csv_fields_are_quoted_and_defused() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("=HYPERLINK(1)"), "'=HYPERLINK(1)");
        assert_eq!(csv_field("-5, cash"), "\"'-5, cash\"");
    }

    #[test]
    fn circulation_days_fill_gaps_and_carry_totals() {
        const DAY: u64 = 86_400;
        let now = 10 * DAY + 3_600;
        let tickets = vec![
            closed(
                "0198c0ef-3f11-7abc-9def-aaaaaa000001",
                TicketKind::Incoming,
                100,
                TicketStatus::Paid,
                2 * DAY,
            ),
            closed(
                "0198c0ef-3f11-7abc-9def-aaaaaa000002",
                TicketKind::Incoming,
                50,
                TicketStatus::Paid,
                8 * DAY + 10,
            ),
            closed(
                "0198c0ef-3f11-7abc-9def-aaaaaa000003",
                TicketKind::Outgoing,
                30,
                TicketStatus::Paid,
                10 * DAY,
            ),
            closed(
                "0198c0ef-3f11-7abc-9def-aaaaaa000004",
                TicketKind::Incoming,
                999,
                TicketStatus::Failed,
                9 * DAY,
            ),
        ];

        let all = circulation_days(&tickets, None, 0, now);
        assert_eq!(all.len(), 9, "day 2 through day 10, gaps included");
        assert_eq!(all[0].circulation, 100);
        assert_eq!(all.last().unwrap().circulation, 120);

        let recent = circulation_days(&tickets, Some(3), 0, now);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].start, 8 * DAY as i64);
        assert_eq!((recent[0].deposits, recent[0].deposit_count), (50, 1));
        assert_eq!(recent[0].circulation, 150, "day 2 carried in");
        assert_eq!((recent[2].payouts, recent[2].payout_count), (30, 1));

        // Two hours west of UTC it is still day 9, and the payout at day 10
        // 00:00 UTC happened on (local) day 9 too.
        let west = circulation_days(&tickets, Some(3), -7_200, now);
        assert_eq!(west[2].start, 9 * DAY as i64 + 7_200);
        assert_eq!(west[2].payouts, 30);

        assert!(circulation_days(&[], Some(7), 0, now).is_empty());
    }

    #[test]
    fn https_detection_follows_the_first_forwarded_proto() {
        assert!(!request_is_https(&HeaderMap::new()));
        assert!(request_is_https(&headers_with_proto("https")));
        assert!(request_is_https(&headers_with_proto("HTTPS")));
        assert!(request_is_https(&headers_with_proto(" https , http")));
        assert!(!request_is_https(&headers_with_proto("http")));
        assert!(!request_is_https(&headers_with_proto("http, https")));
    }

    #[test]
    fn session_cookies_carry_secure_only_over_https() {
        assert_eq!(
            session_cookie("sid-1", false),
            "branch_session=sid-1; Path=/; HttpOnly; SameSite=Lax"
        );
        assert_eq!(
            session_cookie("sid-1", true),
            "branch_session=sid-1; Path=/; HttpOnly; SameSite=Lax; Secure"
        );
        assert_eq!(
            clear_session_cookie(false),
            "branch_session=deleted; Path=/; Max-Age=0"
        );
        assert_eq!(
            clear_session_cookie(true),
            "branch_session=deleted; Path=/; Max-Age=0; Secure"
        );
    }
}
