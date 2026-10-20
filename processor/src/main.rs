//! cdk-branch-processor
//!
//! Single binary that runs:
//!   * a gRPC `cdk-payment-processor` server on $CDK_BRANCH_PROCESSOR_GRPC_PORT,
//!     implementing the "branch" custom payment method for one configured unit.
//!     The operator's own cdk-mintd connects to this and routes all mint/melt
//!     for that method to us.
//!   * a web UI on $CDK_BRANCH_PROCESSOR_HTTP_PORT for branch operators to sign
//!     in (username + password from the users.json store; first boot seeds a
//!     demo admin/admin account), match wallet-created quotes by quote id, and
//!     mark them paid when physical cash is exchanged — plus a Mint tab that
//!     verifies the attached mint's configuration and says what to fix.
//!
//! The processor never writes mint configuration. It attaches to exactly one
//! existing cdk-mintd: the operator completes setup in the console (unit +
//! mint URL), applies the generated config snippet to their mintd, and the
//! attachment checklist plus an end-to-end self-test confirm the link.

mod backend;
mod checks;
mod clients;
mod config;
mod keysets;
mod sessions;
mod state;
mod timefmt;
mod users;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use cdk_common::nuts::CurrencyUnit;
use cdk_payment_processor::PaymentProcessorServer;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tracing_subscriber::EnvFilter;

use crate::backend::BranchBackend;
use crate::checks::SelfTestOutcome;
use crate::clients::MintHttpClient;
use crate::config::{AppConfig, ConfigStore, LoadedConfig, LEGACY_BACKUP_FILENAME};
use crate::keysets::KeysetLog;
use crate::sessions::SessionStore;
use crate::state::BranchState;
use crate::users::UserStore;

const ENV_WORK_DIR: &str = "CDK_BRANCH_PROCESSOR_WORK_DIR";
const ENV_CONFIG_DIR: &str = "CDK_BRANCH_PROCESSOR_CONFIG_DIR";
const ENV_GRPC_ADDR: &str = "CDK_BRANCH_PROCESSOR_GRPC_ADDR";
const ENV_GRPC_PORT: &str = "CDK_BRANCH_PROCESSOR_GRPC_PORT";
const ENV_HTTP_ADDR: &str = "CDK_BRANCH_PROCESSOR_HTTP_ADDR";
const ENV_HTTP_PORT: &str = "CDK_BRANCH_PROCESSOR_HTTP_PORT";
/// Optional directory with `server.pem`, `server.key`, and `ca.pem`: serves
/// the payment gRPC endpoint over mutual TLS (the mint then sets
/// `[grpc_processor] tls_dir` instead of `allow_insecure`). Unset = plaintext.
const ENV_TLS_DIR: &str = "CDK_BRANCH_PROCESSOR_TLS_DIR";
/// First-boot provisioning knob for the installer: seeds the "admin" account
/// with this password instead of the demo credentials, but only while no
/// users.json exists. Ignored ever after — not a password reset. Empty or
/// whitespace-only counts as unset.
const ENV_INITIAL_ADMIN_PASSWORD: &str = "CDK_BRANCH_PROCESSOR_INITIAL_ADMIN_PASSWORD";
/// First-boot provisioning knobs for the installer: pre-seed the Mint-tab
/// attachment (unit / mint URL / advertised gRPC endpoint) so a bundled or
/// flag-attached install boots already attached. Consumed only while no
/// setup.json exists — ignored ever after, not a re-attachment mechanism.
/// Empty counts as unset; an invalid value refuses to boot (a headless
/// install that silently booted unattached would strand the operator).
const ENV_INITIAL_UNIT: &str = "CDK_BRANCH_PROCESSOR_INITIAL_UNIT";
const ENV_INITIAL_MINT_URL: &str = "CDK_BRANCH_PROCESSOR_INITIAL_MINT_URL";
const ENV_INITIAL_ADVERTISED_GRPC: &str = "CDK_BRANCH_PROCESSOR_INITIAL_ADVERTISED_GRPC";
/// The host-published gRPC port as seen from outside the container (compose
/// may remap 50051). Only feeds the console's attachment prefill and snippet
/// guidance; defaults to the bound port.
const ENV_PUBLISHED_GRPC_PORT: &str = "CDK_BRANCH_PROCESSOR_PUBLISHED_GRPC_PORT";
/// Image/build version stamped by the Dockerfile (git tag or edge-<sha>);
/// surfaces in /healthz and the console. "dev" when unset.
const ENV_VERSION: &str = "CDK_BRANCH_PROCESSOR_VERSION";

const DEFAULT_WORK_DIR: &str = "/var/lib/cdk-branch-processor";
const DEFAULT_CONFIG_DIR: &str = "/var/lib/pecan/config";
const DEFAULT_GRPC_ADDR: &str = "0.0.0.0";
const DEFAULT_GRPC_PORT: u16 = 50051;
const DEFAULT_HTTP_ADDR: &str = "0.0.0.0";
const DEFAULT_HTTP_PORT: u16 = 9090;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,h2=warn,hyper=warn,reqwest=warn")),
        )
        .with_ansi(false)
        .init();

    let work_dir =
        PathBuf::from(std::env::var(ENV_WORK_DIR).unwrap_or_else(|_| DEFAULT_WORK_DIR.into()));
    tokio::fs::create_dir_all(&work_dir)
        .await
        .with_context(|| format!("create work_dir {}", work_dir.display()))?;
    let config_dir =
        PathBuf::from(std::env::var(ENV_CONFIG_DIR).unwrap_or_else(|_| DEFAULT_CONFIG_DIR.into()));
    tokio::fs::create_dir_all(&config_dir)
        .await
        .with_context(|| format!("create config_dir {}", config_dir.display()))?;
    // Credentials live on the durable config volume next to setup.json;
    // sessions are operational scratch and live in the work dir.
    let users_path = config_dir.join("users.json");
    let sessions_path = work_dir.join("sessions.json");
    // The managed-stack era mirrored setup.json into the work dir; register
    // it as a legacy migration source for installs upgrading mid-loss.
    let config_store = ConfigStore::new(config_dir)
        .with_legacy_mirror(work_dir.join("managed-stack-backup.json"));

    let grpc_addr = std::env::var(ENV_GRPC_ADDR).unwrap_or_else(|_| DEFAULT_GRPC_ADDR.into());
    let grpc_port: u16 = std::env::var(ENV_GRPC_PORT)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_GRPC_PORT);
    let http_addr = std::env::var(ENV_HTTP_ADDR).unwrap_or_else(|_| DEFAULT_HTTP_ADDR.into());
    let http_port: u16 = std::env::var(ENV_HTTP_PORT)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_HTTP_PORT);
    let http_socket = SocketAddr::from_str(&format!("{http_addr}:{http_port}"))?;
    let tls_dir = std::env::var(ENV_TLS_DIR)
        .ok()
        .map(|dir| dir.trim().to_string())
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);

    let initial_admin_password = std::env::var(ENV_INITIAL_ADMIN_PASSWORD)
        .ok()
        .filter(|password| !password.trim().is_empty());
    let env_trimmed = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let initial_unit = env_trimmed(ENV_INITIAL_UNIT);
    let initial_mint_url = env_trimmed(ENV_INITIAL_MINT_URL);
    let initial_advertised_grpc = env_trimmed(ENV_INITIAL_ADVERTISED_GRPC);
    let published_grpc_port: u16 = std::env::var(ENV_PUBLISHED_GRPC_PORT)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(grpc_port);
    let version = std::env::var(ENV_VERSION)
        .ok()
        .filter(|version| !version.trim().is_empty())
        .unwrap_or_else(|| "dev".to_string());

    let state_path = work_dir.join("tickets.json");
    let keyset_log_path = work_dir.join("keysets.json");

    // Load the config, bootstrap a fresh one, or migrate a v3 managed-stack
    // file. Ordering matters for crash safety: the operator password from a
    // legacy file is seeded into users.json BEFORE the legacy file is
    // replaced, so the hash is never in zero places.
    let (mut migrated, mut fresh) = (None, false);
    let has_initial_attachment =
        initial_unit.is_some() || initial_mint_url.is_some() || initial_advertised_grpc.is_some();
    let app_config = match config_store.load().await? {
        None => {
            fresh = true;
            let mut bootstrapped = config::bootstrap_config(unix_now());
            config::apply_initial_attachment(
                &mut bootstrapped,
                initial_unit.as_deref(),
                initial_mint_url.as_deref(),
                initial_advertised_grpc.as_deref(),
            )
            .with_context(|| {
                format!(
                    "{ENV_INITIAL_UNIT} / {ENV_INITIAL_MINT_URL} / {ENV_INITIAL_ADVERTISED_GRPC}"
                )
            })?;
            bootstrapped
        }
        Some(loaded) => {
            if has_initial_attachment {
                tracing::info!(
                    "setup.json already exists; {ENV_INITIAL_UNIT}/{ENV_INITIAL_MINT_URL}/\
                     {ENV_INITIAL_ADVERTISED_GRPC} are first-boot-only and were ignored"
                );
            }
            match loaded {
                LoadedConfig::Current(config) => config,
                LoadedConfig::Legacy {
                    config,
                    auth_hash,
                    raw,
                } => {
                    migrated = Some((auth_hash, raw));
                    config
                }
            }
        }
    };
    let legacy_auth_hash = migrated.as_ref().and_then(|(hash, _)| hash.clone());
    let users = UserStore::load(users_path, legacy_auth_hash, initial_admin_password).await?;
    if let Some((_, legacy_raw)) = migrated {
        config_store
            .finish_migration(&app_config, &legacy_raw)
            .await?;
        tracing::info!(
            "migrated managed-stack configuration to v4; the old setup.json (which contains \
             the previously-bundled mint's recovery mnemonic) is preserved as {}",
            LEGACY_BACKUP_FILENAME
        );
    } else if fresh {
        config_store.save(&app_config).await?;
        if has_initial_attachment {
            tracing::info!(
                "bootstrapped new configuration at {} with attachment pre-seeded from the \
                 environment (unit={}, mint_url={})",
                config_store.app_config_path().display(),
                app_config.unit,
                app_config.mint_url,
            );
        } else {
            tracing::info!(
                "bootstrapped new configuration at {}; complete setup in the console",
                config_store.app_config_path().display()
            );
        }
    }
    let sessions = SessionStore::load(sessions_path).await?;
    let branch = BranchState::load(state_path).await?;
    let keyset_log = KeysetLog::load(keyset_log_path).await?;

    let unit: Option<CurrencyUnit> = if app_config.unit.is_empty() {
        None
    } else {
        Some(
            app_config
                .unit
                .parse()
                .map_err(|e| anyhow!("bad configured unit {}: {e}", app_config.unit))?,
        )
    };
    let backend = Arc::new(BranchBackend::new(
        branch.clone(),
        unit,
        app_config.method.clone(),
    ));
    let mut server = PaymentProcessorServer::new(backend.clone(), &grpc_addr, grpc_port)
        .map_err(|e| anyhow!("payment processor server init: {e}"))?;
    server
        .start(tls_dir.clone())
        .await
        .map_err(|e| anyhow!("grpc start: {e}"))?;
    tracing::info!(
        "branch-processor gRPC on {grpc_addr}:{grpc_port} (method={}, unit={}, tls={})",
        app_config.method,
        if app_config.unit.is_empty() {
            "not set up yet"
        } else {
            &app_config.unit
        },
        if tls_dir.is_some() { "on" } else { "off" },
    );
    let grpc_server = Some(server);

    let config = Arc::new(RwLock::new(app_config));
    let self_test: Arc<RwLock<Option<SelfTestOutcome>>> = Arc::new(RwLock::new(None));
    let self_test_running = Arc::new(AtomicBool::new(false));

    spawn_ticket_sweeper(branch.clone());
    spawn_keyset_observer(config.clone(), keyset_log.clone(), branch.clone());
    spawn_self_test_on_first_attach(
        config.clone(),
        config_store.clone(),
        branch.clone(),
        backend.clone(),
        self_test.clone(),
        self_test_running.clone(),
    );

    let app = web::router(web::WebState::new(
        branch,
        backend,
        config,
        config_store,
        users,
        sessions,
        version,
        format!("{grpc_addr}:{grpc_port}"),
        tls_dir.is_some(),
        published_grpc_port,
        self_test,
        self_test_running,
        keyset_log,
    ));

    tracing::info!("branch-processor HTTP on {http_socket}");
    let listener = TcpListener::bind(http_socket).await?;
    let serve = axum::serve(listener, app);

    tokio::select! {
        r = serve => r.map_err(|e| anyhow!("http: {e}"))?,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl-c received, shutting down");
        }
    }

    if let Some(server) = grpc_server {
        let _ = server.stop().await;
    }
    Ok(())
}

/// Periodically drop expired tickets no money ever moved for, so abandoned
/// wallet-created quotes neither clutter the teller's open list nor pin the
/// open-quote cap. Funded melts are never touched (see `sweep_expired`).
fn spawn_ticket_sweeper(branch: BranchState) {
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(60)).await;
            let removed = branch.sweep_expired().await;
            if removed > 0 {
                tracing::info!("swept {removed} expired unfunded quote(s)");
            }
        }
    });
}

/// Read the mint's keyset list periodically so the console's key timeline
/// knows when keysets appear and stop issuing, even while nobody has the
/// console open. Rotations are rare; a few minutes of resolution is plenty.
fn spawn_keyset_observer(
    config: Arc<RwLock<AppConfig>>,
    keyset_log: KeysetLog,
    branch: BranchState,
) {
    tokio::spawn(async move {
        loop {
            let mint_url = config.read().await.mint_url.clone();
            if !mint_url.is_empty() {
                match MintHttpClient::new(mint_url).list_keysets().await {
                    Ok(keysets) => {
                        if keyset_log.observe(&keysets, unix_now()).await {
                            branch.notify_ui_change();
                        }
                    }
                    Err(e) => tracing::debug!("keyset observation skipped: {e:#}"),
                }
            }
            sleep(Duration::from_secs(300)).await;
        }
    });
}

/// Run the end-to-end self-test automatically, once, when the mint first
/// attaches to the payment stream — so a fresh install's checklist completes
/// itself the moment the operator's mintd comes up. Manual runs from the
/// console take precedence; a successful run locks the unit.
fn spawn_self_test_on_first_attach(
    config: Arc<RwLock<AppConfig>>,
    config_store: ConfigStore,
    branch: BranchState,
    backend: Arc<BranchBackend>,
    self_test: Arc<RwLock<Option<SelfTestOutcome>>>,
    running: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(5)).await;
            if self_test.read().await.is_some() {
                // A run (manual or ours) already happened this boot.
                return;
            }
            if backend.stream_attached_at().is_none() {
                continue;
            }
            let snapshot = config.read().await.clone();
            if !snapshot.setup_complete() {
                continue;
            }
            if running.swap(true, Ordering::SeqCst) {
                continue; // manual run in flight; check again next tick
            }
            tracing::info!("mint attached — running the end-to-end self-test");
            let outcome = checks::run_self_test(
                &MintHttpClient::new(snapshot.mint_url.clone()),
                &branch,
                &snapshot.method,
                &snapshot.unit,
            )
            .await;
            let succeeded = outcome.ok;
            *self_test.write().await = Some(outcome);
            running.store(false, Ordering::SeqCst);
            if succeeded {
                let mut updated = config.write().await;
                if !updated.unit_locked {
                    updated.lock_unit();
                    if let Err(e) = config_store.save(&updated).await {
                        tracing::warn!("could not persist unit lock: {e:#}");
                    }
                }
            }
            branch.notify_ui_change();
            return;
        }
    });
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
