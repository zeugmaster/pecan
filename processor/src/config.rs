//! Processor configuration: one unit, one attached mint.
//!
//! The processor never writes mint configuration. `setup.json` holds only what
//! this process needs to serve the "branch" payment method and to verify the
//! attached mint: the unit, the mint's public URL, and how the mint reaches
//! our gRPC endpoint (rendered into the config snippet the mint's operator
//! applies by hand). First boot bootstraps an empty config with zero
//! interaction; the operator completes setup in the console.
//!
//! Version 3 files (the managed-stack era: mnemonic, managed units, rollover
//! policies, mint.toml rendering) are migrated on load. The old file — which
//! contains the recovery mnemonic — is preserved verbatim next to the new one
//! and never deleted; the seed belongs to whoever operates the mint now.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use bitcoin_hashes::{sha256, Hash};
use serde::{Deserialize, Serialize};

pub const PASSWORD_MIN_LENGTH: usize = 8;

/// The tested CDK release for wallet authentication. What actually
/// gates the link is strict equality of the payment-processor protocol
/// version ([`cdk_common::PAYMENT_PROCESSOR_PROTOCOL_VERSION`]); this release
/// is pinned in processor/Cargo.toml. Keep the versions aligned —
/// CI's pin-check enforces the pair.
pub const COMPATIBLE_CDK_VERSION: &str = "0.18.2";

/// The official docker image operators can run instead of building cdk-mintd
/// themselves.
pub fn compatible_mintd_image() -> String {
    format!("cashubtc/mintd:{COMPATIBLE_CDK_VERSION}")
}

/// Bookkeeping lifetime for a melt ticket the wallet never funds. The mint's
/// own melt-quote TTL governs the wallet; this only bounds how long an
/// unfunded payout row stays in the teller's open list before the sweeper
/// reclaims it.
pub const MELT_TICKET_TTL_SECS: u64 = 15 * 60;

/// Filename the pre-rescope (v3, "managed mint") setup.json is preserved
/// under. It contains the recovery mnemonic of the formerly-bundled mint and
/// must never be deleted by this software.
pub const LEGACY_BACKUP_FILENAME: &str = "setup.json.v3-managed.bak";

#[derive(Clone, Debug)]
pub struct ConfigStore {
    app_config_path: PathBuf,
    legacy_backup_target: PathBuf,
    /// The managed-stack era mirrored setup.json into the work dir; it is
    /// read (only) as a legacy migration source when setup.json is missing.
    legacy_mirror_path: Option<PathBuf>,
}

/// What `ConfigStore::load` found on disk.
pub enum LoadedConfig {
    /// A current (v4) configuration.
    Current(AppConfig),
    /// A v3 managed-stack configuration, already converted. The caller must
    /// seed the user store from `auth_hash` (if any) **before** calling
    /// [`ConfigStore::finish_migration`], so a crash mid-migration never
    /// leaves the operator password in zero places.
    Legacy {
        config: AppConfig,
        auth_hash: Option<String>,
        raw: Vec<u8>,
    },
}

impl ConfigStore {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            app_config_path: config_dir.join("setup.json"),
            legacy_backup_target: config_dir.join(LEGACY_BACKUP_FILENAME),
            legacy_mirror_path: None,
        }
    }

    /// Register the managed-stack era mirror file (work dir) as an additional
    /// legacy migration source.
    pub fn with_legacy_mirror(mut self, path: PathBuf) -> Self {
        self.legacy_mirror_path = Some(path);
        self
    }

    pub fn app_config_path(&self) -> &Path {
        &self.app_config_path
    }

    pub async fn load(&self) -> Result<Option<LoadedConfig>> {
        let mut sources = vec![self.app_config_path.clone()];
        if let Some(mirror) = &self.legacy_mirror_path {
            sources.push(mirror.clone());
        }
        for path in sources {
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                continue;
            }
            let raw = tokio::fs::read(&path)
                .await
                .with_context(|| format!("read {}", path.display()))?;
            return Ok(Some(
                parse_config(&raw).with_context(|| format!("parse {}", path.display()))?,
            ));
        }
        Ok(None)
    }

    pub async fn save(&self, config: &AppConfig) -> Result<()> {
        config.validate()?;
        if let Some(parent) = self.app_config_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let bytes = serde_json::to_vec_pretty(config)?;
        write_atomic(&self.app_config_path, &bytes).await
    }

    /// Preserve the legacy v3 file verbatim, then persist the converted
    /// config. The backup is written first and never overwritten once it
    /// exists — it holds the old mint's recovery mnemonic.
    pub async fn finish_migration(&self, config: &AppConfig, legacy_raw: &[u8]) -> Result<()> {
        if let Some(parent) = self.legacy_backup_target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if !tokio::fs::try_exists(&self.legacy_backup_target)
            .await
            .unwrap_or(false)
        {
            write_atomic(&self.legacy_backup_target, legacy_raw).await?;
        }
        self.save(config).await
    }
}

fn parse_config(raw: &[u8]) -> Result<LoadedConfig> {
    #[derive(Deserialize)]
    struct VersionProbe {
        #[serde(default)]
        version: u32,
    }
    let probe: VersionProbe = serde_json::from_slice(raw)?;
    if probe.version >= 4 {
        let config: AppConfig = serde_json::from_slice(raw)?;
        config.validate()?;
        return Ok(LoadedConfig::Current(config));
    }
    let legacy: LegacyConfig = serde_json::from_slice(raw)?;
    let auth_hash = legacy.auth.as_ref().map(|auth| auth.password_hash.clone());
    let config = legacy.into_v4();
    config.validate()?;
    Ok(LoadedConfig::Legacy {
        config,
        auth_hash,
        raw: raw.to_vec(),
    })
}

// ---------------------------------------------------------------------------
// Current configuration (v4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub wallet_auth: Option<crate::mint_auth::AuthDraft>,
    pub version: u32,
    pub configured_at: u64,
    /// The custom payment method this processor implements. Constant in
    /// practice; kept in the file so the value is visible and greppable.
    pub method: String,
    /// The single currency unit this install serves. Empty until the operator
    /// completes setup. Must match the attached mint's `[[payment_backend]]
    /// unit` and `[grpc_processor].supported_units` entry byte-for-byte —
    /// guaranteed by generating the config snippet from this value.
    #[serde(default)]
    pub unit: String,
    /// The attached mint's public HTTP base URL (the same URL wallets use).
    /// Empty = not attached yet.
    #[serde(default)]
    pub mint_url: String,
    /// This processor's gRPC endpoint as reachable from the mint,
    /// `host[:port]` (no scheme). Only used to render the config snippet.
    #[serde(default)]
    pub advertised_grpc: String,
    /// Set once the unit has been exercised (first successful self-test).
    /// A locked unit is read-only in the console: issued ecash and quotes
    /// reference it, so changing it is a documented manual file edit.
    #[serde(default)]
    pub unit_locked: bool,
    /// True when this file was migrated from a v3 managed-stack config; the
    /// console shows a one-time notice pointing at the preserved backup
    /// (which contains the old mint's recovery mnemonic).
    #[serde(default)]
    pub migrated_from_managed: bool,
}

/// Build the first-boot configuration: method fixed, nothing attached, no
/// unit. The console guides the operator through the rest.
pub fn bootstrap_config(configured_at: u64) -> AppConfig {
    AppConfig {
        version: 4,
        wallet_auth: None,
        configured_at,
        method: "branch".to_string(),
        unit: String::new(),
        mint_url: String::new(),
        advertised_grpc: String::new(),
        unit_locked: false,
        migrated_from_managed: false,
    }
}

/// Apply the installer's first-boot attachment environment to a freshly
/// bootstrapped config. Values pass the same validation as the console form;
/// an invalid value is a hard error — a headless install that silently booted
/// unattached would strand the operator with nothing but a log line, while a
/// refusal surfaces in the installer's health wait.
pub fn apply_initial_attachment(
    config: &mut AppConfig,
    unit: Option<&str>,
    mint_url: Option<&str>,
    advertised_grpc: Option<&str>,
) -> Result<()> {
    if unit.is_none() && mint_url.is_none() && advertised_grpc.is_none() {
        return Ok(());
    }
    config.set_attachment(unit, mint_url.unwrap_or(""), advertised_grpc.unwrap_or(""))
}

impl AppConfig {
    pub fn validate(&self) -> Result<()> {
        if let Some(draft) = &self.wallet_auth {
            draft.validate()?;
        }
        if self.method.trim().is_empty() {
            bail!("payment method is required");
        }
        if !self.unit.is_empty() {
            validate_slug("unit", &self.unit)?;
        }
        if !self.mint_url.is_empty() {
            validate_http_url("mint URL", &self.mint_url)?;
        }
        if !self.advertised_grpc.is_empty() {
            validate_grpc_endpoint(&self.advertised_grpc)?;
        }
        Ok(())
    }

    pub fn is_attached(&self) -> bool {
        !self.mint_url.is_empty()
    }

    pub fn setup_complete(&self) -> bool {
        !self.unit.is_empty() && self.is_attached()
    }

    /// Apply the console's attachment form. `unit` is `None` to keep the
    /// current unit. Changing the unit is refused once it is locked; the
    /// caller additionally guards on existing tickets.
    pub fn set_attachment(
        &mut self,
        unit: Option<&str>,
        mint_url: &str,
        advertised_grpc: &str,
    ) -> Result<()> {
        if let Some(unit) = unit {
            let unit = unit.trim().to_ascii_lowercase();
            if unit != self.unit {
                if self.unit_locked {
                    bail!(
                        "the unit is locked: ecash and quotes reference {}; \
                         see the operations guide before changing it",
                        self.unit
                    );
                }
                validate_slug("unit", &unit)?;
                self.unit = unit;
            }
        }
        let mint_url = mint_url.trim().trim_end_matches('/');
        if !mint_url.is_empty() {
            validate_http_url("mint URL", mint_url)?;
        }
        self.mint_url = mint_url.to_string();
        let advertised = normalize_grpc_endpoint(advertised_grpc)?;
        self.advertised_grpc = advertised;
        Ok(())
    }

    pub fn lock_unit(&mut self) {
        if !self.unit.is_empty() {
            self.unit_locked = true;
        }
    }
}

// ---------------------------------------------------------------------------
// The mint-side config snippet
// ---------------------------------------------------------------------------

/// Render the `mint.toml` fragment the mint's operator merges into their
/// cdk-mintd config. `grpc_tls` reflects whether THIS processor serves TLS
/// (CDK_BRANCH_PROCESSOR_TLS_DIR): it decides between `tls_dir` and
/// `allow_insecure` on the mint side. Returns `None` until the unit and the
/// advertised gRPC endpoint are configured.
pub fn render_mint_snippet(config: &AppConfig, grpc_tls: bool) -> Option<String> {
    if config.unit.is_empty() || config.advertised_grpc.is_empty() {
        return None;
    }
    let (host, port) = split_host_port(&config.advertised_grpc);
    let transport = if grpc_tls {
        "# This processor serves TLS: point tls_dir at a directory containing\n\
         # ca.pem plus a client certificate/key issued by that CA.\n\
         tls_dir = \"/path/to/processor-tls\""
            .to_string()
    } else {
        "# The gRPC link is plaintext — keep it on the same host or a private\n\
         # network, never the open internet.\n\
         allow_insecure = true"
            .to_string()
    };
    Some(format!(
        r#"# Branch settlement backend — add to your cdk-mintd's stored configuration.
#
# cdk-mintd {version} keeps its configuration in the mint database:
#   fresh mint:    cdk-mintd config init --new-mint --file mint.toml   (then start it)
#   running mint:  cdk-mintd config export --file mint.toml
#                  …merge this snippet into the exported file…
#                  cdk-mintd config apply --file mint.toml
#                  then restart cdk-mintd
# Editing a mint.toml on disk without `config apply` changes nothing.
#
# Tested with cdk-mintd v{version} (docker image {image}) —
# requires payment-processor protocol {proto}. Mismatched protocol versions
# are rejected at connect time.

[[payment_backend]]
backend = "grpcprocessor"
unit = "{unit}"
min_mint = 1
max_mint = 500000
min_melt = 1
max_melt = 500000

[grpc_processor]
supported_units = ["{unit}"]
address = "{host}"
port = {port}
{transport}

# Counter-friendly quote lifetimes (cdk's defaults: 3600 s deposits, 60 s
# payouts — a wallet's confirmation screen misses a 60 s melt window).
[info.quote_ttl]
mint_ttl = 1800
melt_ttl = 900
"#,
        version = COMPATIBLE_CDK_VERSION,
        image = compatible_mintd_image(),
        proto = cdk_common::PAYMENT_PROCESSOR_PROTOCOL_VERSION,
        unit = toml_escape(&config.unit),
        host = toml_escape(&host),
        port = port,
        transport = transport,
    ))
}

/// `host[:port]` → (`host`, port), defaulting the port to 50051. Any scheme
/// prefix was already stripped by [`normalize_grpc_endpoint`].
fn split_host_port(endpoint: &str) -> (String, u16) {
    if let Some((host, port)) = endpoint.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            return (host.to_string(), port);
        }
    }
    (endpoint.to_string(), 50051)
}

/// Accept `host`, `host:port`, or a full `http(s)://host:port`, and store the
/// bare `host[:port]` form (cdk-mintd's `[grpc_processor].address` is a host,
/// not a URL).
fn normalize_grpc_endpoint(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let bare = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    validate_grpc_endpoint(bare)?;
    Ok(bare.to_string())
}

fn validate_grpc_endpoint(endpoint: &str) -> Result<()> {
    if endpoint.is_empty() {
        bail!("processor gRPC endpoint is required");
    }
    if endpoint.contains(char::is_whitespace) || endpoint.contains('/') {
        bail!("processor gRPC endpoint must be host or host:port, e.g. 10.0.0.5:50051");
    }
    if let Some((host, port)) = endpoint.rsplit_once(':') {
        if host.is_empty() {
            bail!("processor gRPC endpoint needs a host, e.g. 10.0.0.5:50051");
        }
        port.parse::<u16>()
            .map_err(|_| anyhow!("processor gRPC port must be a number, got {port:?}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Legacy (v3, managed stack) migration
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LegacyConfig {
    #[serde(default)]
    configured_at: u64,
    mint: LegacyMintSetup,
    #[serde(default)]
    auth: Option<LegacyAuthConfig>,
    #[serde(default)]
    endpoints: Option<LegacyEndpoints>,
    #[serde(default)]
    units: Vec<LegacyUnit>,
    #[serde(default)]
    mint_connection: LegacyConnection,
}

#[derive(Debug, Deserialize)]
struct LegacyMintSetup {
    #[serde(default)]
    unit: String,
    #[serde(default = "default_method")]
    method: String,
    // The mnemonic is deliberately not deserialized: it stays only in the
    // preserved raw bytes of the old file.
}

fn default_method() -> String {
    "branch".to_string()
}

#[derive(Debug, Deserialize)]
struct LegacyAuthConfig {
    password_hash: String,
}

#[derive(Debug, Deserialize)]
struct LegacyEndpoints {
    #[serde(default)]
    public_url: String,
    #[serde(default)]
    processor_grpc_addr: String,
    #[serde(default)]
    processor_grpc_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct LegacyUnit {
    unit: String,
    #[serde(default)]
    lifecycle: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum LegacyConnection {
    #[default]
    Bundled,
    Unset,
    External {
        #[serde(default)]
        http_url: String,
        #[serde(default)]
        advertised_grpc: String,
    },
}

impl LegacyConfig {
    fn into_v4(self) -> AppConfig {
        // Primary unit first; otherwise the first unit that was not retired.
        let unit = if !self.mint.unit.is_empty() {
            self.mint.unit.clone()
        } else {
            self.units
                .iter()
                .find(|unit| unit.lifecycle.as_deref() != Some("retired"))
                .map(|unit| unit.unit.clone())
                .unwrap_or_default()
        };
        let dropped: Vec<&str> = self
            .units
            .iter()
            .map(|u| u.unit.as_str())
            .filter(|u| *u != unit)
            .collect();
        if !dropped.is_empty() {
            tracing::warn!(
                "migrating multi-unit managed config: keeping {unit:?}, dropping units {:?} \
                 (their historical tickets remain; new quotes are single-unit)",
                dropped
            );
        }
        let (mint_url, advertised_grpc) = match &self.mint_connection {
            LegacyConnection::External {
                http_url,
                advertised_grpc,
            } => (
                http_url.clone(),
                normalize_grpc_endpoint(advertised_grpc).unwrap_or_default(),
            ),
            LegacyConnection::Bundled => {
                let endpoints = self.endpoints.as_ref();
                let mint_url = endpoints
                    .map(|e| e.public_url.trim_end_matches('/').to_string())
                    .unwrap_or_default();
                let advertised = endpoints
                    .map(|e| {
                        let port = e.processor_grpc_port.unwrap_or(50051);
                        format!("{}:{port}", e.processor_grpc_addr)
                    })
                    .and_then(|endpoint| normalize_grpc_endpoint(&endpoint).ok())
                    .unwrap_or_default();
                (mint_url, advertised)
            }
            LegacyConnection::Unset => (String::new(), String::new()),
        };
        AppConfig {
            version: 4,
            wallet_auth: None,
            configured_at: self.configured_at,
            method: self.mint.method,
            // A managed install with a unit has (or had) ecash under it:
            // lock conservatively so the migrated console cannot re-point it.
            unit_locked: !unit.is_empty(),
            unit,
            mint_url,
            advertised_grpc,
            migrated_from_managed: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_slug(label: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{label} is required");
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        bail!("{label} may only contain lowercase letters, digits, hyphen, and underscore");
    }
    Ok(())
}

fn validate_http_url(label: &str, url: &str) -> Result<()> {
    let url = url.trim();
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| anyhow!("{label} must start with http:// or https://"))?;
    if rest.is_empty() || rest.starts_with('/') {
        bail!("{label} needs a host, e.g. https://mint.example.org");
    }
    if url.contains(char::is_whitespace) {
        bail!("{label} must not contain whitespace");
    }
    Ok(())
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ---------------------------------------------------------------------------
// Passwords (unchanged from the managed era)
// ---------------------------------------------------------------------------

/// Length is the only strength requirement — no composition rules.
pub fn validate_operator_password(password: &str, password_confirm: &str) -> Result<()> {
    if password.chars().count() < PASSWORD_MIN_LENGTH {
        bail!("operator password must be at least {PASSWORD_MIN_LENGTH} characters");
    }
    if password != password_confirm {
        bail!("operator passwords do not match");
    }
    Ok(())
}

// Upgrade slot: to move to a vetted KDF (argon2id), add an "argon2id:" branch
// in `verify_password`, switch this function's output format, and rehash
// opportunistically on successful login in `UserStore::verify`.
pub fn hash_password(password: &str) -> String {
    let salt = uuid::Uuid::new_v4().to_string();
    let digest = password_digest(&salt, password);
    format!("sha256:120000:{salt}:{digest}")
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split(':').collect();
    if parts.len() != 4 || parts[0] != "sha256" {
        return false;
    }
    let iterations = parts[1].parse::<usize>().unwrap_or(0);
    if iterations == 0 {
        return false;
    }
    let digest = password_digest_with_iterations(parts[2], password, iterations);
    constant_time_eq(digest.as_bytes(), parts[3].as_bytes())
}

fn password_digest(salt: &str, password: &str) -> String {
    password_digest_with_iterations(salt, password, 120_000)
}

fn password_digest_with_iterations(salt: &str, password: &str, iterations: usize) -> String {
    let mut input = format!("{salt}:{password}").into_bytes();
    let mut hash = sha256::Hash::hash(&input).to_byte_array().to_vec();
    for _ in 1..iterations {
        input.clear();
        input.extend_from_slice(salt.as_bytes());
        input.push(b':');
        input.extend_from_slice(&hash);
        hash = sha256::Hash::hash(&input).to_byte_array().to_vec();
    }
    bytes_to_hex(&hash)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub(crate) async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .with_context(|| format!("write {}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_is_valid_and_unattached() {
        let config = bootstrap_config(42);
        assert!(config.validate().is_ok());
        assert_eq!(config.version, 4);
        assert_eq!(config.method, "branch");
        assert!(!config.is_attached());
        assert!(!config.setup_complete());
        assert!(render_mint_snippet(&config, false).is_none());
    }

    #[test]
    fn attachment_setup_and_snippet() {
        let mut config = bootstrap_config(1);
        config
            .set_attachment(
                Some("ORA"),
                "https://mint.example.org/",
                "http://10.0.0.5:50051",
            )
            .expect("setup");
        assert_eq!(config.unit, "ora");
        assert_eq!(config.mint_url, "https://mint.example.org");
        assert_eq!(config.advertised_grpc, "10.0.0.5:50051");
        assert!(config.setup_complete());

        let snippet = render_mint_snippet(&config, false).expect("snippet");
        assert!(snippet.contains("unit = \"ora\""));
        assert!(snippet.contains("supported_units = [\"ora\"]"));
        assert!(snippet.contains("address = \"10.0.0.5\""));
        assert!(snippet.contains("port = 50051"));
        assert!(snippet.contains("allow_insecure = true"));
        assert!(snippet.contains("[[payment_backend]]"));
        assert!(snippet.contains("backend = \"grpcprocessor\""));
        assert!(snippet.contains(COMPATIBLE_CDK_VERSION));
        assert!(!snippet.contains("[[ln]]"));

        let tls = render_mint_snippet(&config, true).expect("tls snippet");
        assert!(tls.contains("tls_dir"));
        assert!(!tls.contains("allow_insecure"));
    }

    #[test]
    fn initial_attachment_applies_and_validates() {
        // Full trio: unit lowercased, URL trailing slash trimmed, scheme
        // stripped from the gRPC endpoint.
        let mut config = bootstrap_config(1);
        apply_initial_attachment(
            &mut config,
            Some("ORA"),
            Some("https://mint.example.org/"),
            Some("http://processor:50051"),
        )
        .expect("initial attachment");
        assert_eq!(config.unit, "ora");
        assert_eq!(config.mint_url, "https://mint.example.org");
        assert_eq!(config.advertised_grpc, "processor:50051");
        assert!(config.setup_complete());

        // Subset: unit only — incomplete but valid, console finishes the rest.
        let mut config = bootstrap_config(1);
        apply_initial_attachment(&mut config, Some("ora"), None, None).expect("unit only");
        assert_eq!(config.unit, "ora");
        assert!(!config.is_attached());
        assert!(config.validate().is_ok());

        // Invalid values are hard errors.
        let mut config = bootstrap_config(1);
        assert!(apply_initial_attachment(&mut config, Some("NO SPACES"), None, None).is_err());
        let mut config = bootstrap_config(1);
        assert!(apply_initial_attachment(&mut config, None, Some("ftp://x"), None).is_err());
        let mut config = bootstrap_config(1);
        assert!(apply_initial_attachment(&mut config, None, None, Some("host/path")).is_err());

        // No vars at all: a plain bootstrap, untouched.
        let mut config = bootstrap_config(7);
        apply_initial_attachment(&mut config, None, None, None).expect("no-op");
        assert!(!config.setup_complete());
        assert_eq!(config.configured_at, 7);
    }

    #[test]
    fn locked_unit_refuses_change_but_urls_stay_editable() {
        let mut config = bootstrap_config(1);
        config
            .set_attachment(Some("ora"), "http://mint:8089", "processor:50051")
            .expect("setup");
        config.lock_unit();
        assert!(config.unit_locked);
        assert!(config
            .set_attachment(Some("usd"), "http://mint:8089", "processor:50051")
            .is_err());
        // Same unit passed back is a no-op, not a violation.
        config
            .set_attachment(Some("ora"), "http://mint-2:8089", "processor:50051")
            .expect("url change with locked unit");
        assert_eq!(config.mint_url, "http://mint-2:8089");
    }

    #[test]
    fn grpc_endpoint_forms_are_normalized() {
        let mut config = bootstrap_config(1);
        for (input, expected) in [
            ("http://10.0.0.5:50051", "10.0.0.5:50051"),
            ("processor", "processor"),
            ("processor:60051/", "processor:60051"),
        ] {
            config
                .set_attachment(None, "http://mint:8089", input)
                .expect(input);
            assert_eq!(config.advertised_grpc, expected);
        }
        assert!(config
            .set_attachment(None, "http://mint:8089", "host:notaport")
            .is_err());
        assert!(config
            .set_attachment(None, "http://mint:8089", "host/path")
            .is_err());
        assert!(config
            .set_attachment(None, "ftp://mint:8089", "processor")
            .is_err());
    }

    const V3_BUNDLED: &str = r#"{
      "version": 3,
      "configured_at": 1700000000,
      "mint": {
        "name": "Branch mint",
        "description": "d",
        "description_long": "dl",
        "unit": "ora",
        "method": "branch",
        "mnemonic": "legal winner thank year wave sausage worth useful legal winner thank yellow"
      },
      "auth": { "password_hash": "sha256:120000:salt:digest" },
      "endpoints": {
        "public_url": "https://mint.example.org",
        "mint_http_url": "http://mint:8089",
        "mint_rpc_url": "http://mint:8091",
        "processor_grpc_addr": "processor",
        "processor_grpc_port": 50051
      },
      "rollover": {
        "enabled": true, "keyset_lifetime_days": 90,
        "rotate_before_expiry_days": 14, "input_fee_ppk": 0, "amounts": [1, 2]
      },
      "units": [
        { "unit": "ora", "lifecycle": "active", "configured_at": 1700000000,
          "rollover": { "enabled": true, "keyset_lifetime_days": 90,
            "rotate_before_expiry_days": 14, "input_fee_ppk": 0, "amounts": [1, 2] } },
        { "unit": "usd", "lifecycle": "retired", "configured_at": 1700000001,
          "rollover": { "enabled": true, "keyset_lifetime_days": 90,
            "rotate_before_expiry_days": 14, "input_fee_ppk": 0, "amounts": [1, 2] } }
      ],
      "seed_fingerprint": "sha256:0011223344556677",
      "mint_connection": { "mode": "bundled" }
    }"#;

    #[test]
    fn v3_bundled_config_migrates() {
        let loaded = parse_config(V3_BUNDLED.as_bytes()).expect("parse");
        let LoadedConfig::Legacy {
            config,
            auth_hash,
            raw,
        } = loaded
        else {
            panic!("expected legacy config");
        };
        assert_eq!(config.version, 4);
        assert_eq!(config.unit, "ora");
        assert!(config.unit_locked, "migrated units with ecash must lock");
        assert!(config.migrated_from_managed);
        assert_eq!(config.mint_url, "https://mint.example.org");
        assert_eq!(config.advertised_grpc, "processor:50051");
        assert_eq!(auth_hash.as_deref(), Some("sha256:120000:salt:digest"));
        // The raw bytes (with the mnemonic) survive for the backup file.
        assert!(String::from_utf8_lossy(&raw).contains("legal winner"));
    }

    #[test]
    fn v3_external_and_unset_configs_migrate() {
        let external = V3_BUNDLED.replace(
            r#""mint_connection": { "mode": "bundled" }"#,
            r#""mint_connection": { "mode": "external",
                "http_url": "http://10.0.0.7:8089",
                "rpc_url": "http://10.0.0.7:8091",
                "advertised_grpc": "http://10.0.0.5:50051" }"#,
        );
        let LoadedConfig::Legacy { config, .. } =
            parse_config(external.as_bytes()).expect("parse external")
        else {
            panic!("expected legacy");
        };
        assert_eq!(config.mint_url, "http://10.0.0.7:8089");
        assert_eq!(config.advertised_grpc, "10.0.0.5:50051");

        let unset = V3_BUNDLED
            .replace(
                r#""mint_connection": { "mode": "bundled" }"#,
                r#""mint_connection": { "mode": "unset" }"#,
            )
            .replace(r#""unit": "ora","#, r#""unit": "","#)
            .replace(
                r#"{ "unit": "ora", "lifecycle": "active""#,
                r#"{ "unit": "ora", "lifecycle": "retired""#,
            );
        let LoadedConfig::Legacy { config, .. } =
            parse_config(unset.as_bytes()).expect("parse unset")
        else {
            panic!("expected legacy");
        };
        assert!(!config.is_attached());
        // Every listed unit is retired → nothing to adopt, nothing to lock.
        assert_eq!(config.unit, "");
        assert!(!config.unit_locked);
    }

    #[test]
    fn v4_files_round_trip() {
        let mut config = bootstrap_config(7);
        config
            .set_attachment(Some("ora"), "http://mint:8089", "processor")
            .expect("setup");
        let raw = serde_json::to_vec_pretty(&config).expect("serialize");
        let LoadedConfig::Current(parsed) = parse_config(&raw).expect("parse") else {
            panic!("expected current config");
        };
        assert_eq!(parsed.unit, "ora");
        assert_eq!(parsed.mint_url, "http://mint:8089");
        assert!(!parsed.migrated_from_managed);
    }

    #[test]
    fn password_rule_is_length_only() {
        assert!(validate_operator_password("1234567", "1234567").is_err()); // 7 chars
        assert!(validate_operator_password("12345678", "12345678").is_ok()); // digits only
        assert!(validate_operator_password("password", "password").is_ok()); // letters only
        assert!(validate_operator_password("password", "different").is_err()); // mismatch
    }

    #[tokio::test]
    async fn store_migration_preserves_the_legacy_file() {
        let dir = std::env::temp_dir().join(format!("cfg-migrate-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let store = ConfigStore::new(dir.clone());
        tokio::fs::write(dir.join("setup.json"), V3_BUNDLED)
            .await
            .unwrap();

        let Some(LoadedConfig::Legacy { config, raw, .. }) = store.load().await.unwrap() else {
            panic!("expected legacy load");
        };
        store.finish_migration(&config, &raw).await.unwrap();

        let backup = tokio::fs::read_to_string(dir.join(LEGACY_BACKUP_FILENAME))
            .await
            .expect("backup exists");
        assert!(backup.contains("legal winner"), "mnemonic preserved");

        // Reload now yields the migrated v4 config…
        let Some(LoadedConfig::Current(reloaded)) = store.load().await.unwrap() else {
            panic!("expected current config after migration");
        };
        assert_eq!(reloaded.unit, "ora");

        // …and re-running the migration never overwrites the backup.
        store
            .finish_migration(&reloaded, b"{\"different\": true}")
            .await
            .unwrap();
        let backup_again = tokio::fs::read_to_string(dir.join(LEGACY_BACKUP_FILENAME))
            .await
            .unwrap();
        assert_eq!(backup, backup_again);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
