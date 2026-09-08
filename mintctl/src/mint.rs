//! The bundled mint: cdk-mintd config rendering and seed generation.
//!
//! In "processor + mint" installs the official cashubtc/mintd image runs
//! beside the processor (compose profile "mint"). cdk-mintd 0.18 stores its
//! configuration in the mint database: the TOML written here is an import
//! document consumed exactly once by `config init` on the mint's first start.
//! Later changes go through `cdk-mintd config apply` (docs/operations.md).

use std::path::Path;

use anyhow::{bail, Context, Result};

/// The cashubtc/mintd image tag new bundled-mint installs pin. Must agree
/// with COMPATIBLE_CDK_VERSION in processor/src/config.rs and the default in
/// docker-compose.yml — CI's pin-check greps all mintd tag references.
pub const MINTD_DEFAULT_TAG: &str = "0.18.0-rc.0";

/// Units wallets treat specially (or that invite confusion with real
/// currencies). Not rejected — a warning nudges the operator toward a name
/// that is unambiguously theirs.
pub const RESERVED_UNITS: &[&str] = &["sat", "msat", "btc", "usd", "eur", "auth"];

/// Generate the mint's BIP39 seed: 12 English words (128-bit entropy, the
/// secp256k1 security level, and short enough to transcribe at a counter).
pub fn generate_mnemonic() -> String {
    bip39::Mnemonic::generate(12)
        .expect("12 is a valid BIP39 word count")
        .to_string()
}

/// The unit rule, mirroring the processor's `validate_slug`
/// (processor/src/config.rs): trimmed, lowercased, `[a-z0-9_-]+`.
pub fn validate_unit_slug(raw: &str) -> Result<String> {
    let unit = raw.trim().to_ascii_lowercase();
    if unit.is_empty() {
        bail!("the unit is required (a short name like \"ora\")");
    }
    if !unit
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        bail!("the unit may only contain lowercase letters, digits, - and _ (got {raw:?})");
    }
    Ok(unit)
}

/// The cdk-mintd import document for a bundled mint: one grpcprocessor
/// payment backend for `unit`, dialing the processor over the compose
/// network, seed as a file: secret reference.
pub fn render_config_toml(unit: &str, mint_url: &str) -> String {
    format!(
        r#"# Pecan — bundled mint (cdk-mintd) import document.
#
# Imported ONCE by `cdk-mintd config init` on the mint's first start; after
# that the mint's database is authoritative and edits here do NOTHING until
# you explicitly apply them:
#
#   docker compose -f <install-dir>/docker-compose.yml \
#     --project-directory <install-dir> \
#     run --rm --no-deps mintd cdk-mintd --work-dir /data config apply --file /config/mint.toml
#   mintctl start
#
# The mint's seed lives in ./mnemonic (0600) — it IS the mint: anyone holding
# it can issue this mint's ecash. mintctl backup includes it; keep copies
# offline.

[info]
url = "{mint_url}"
listen_host = "0.0.0.0"
listen_port = 8085
mnemonic = "file:/run/secrets/mint-mnemonic"

# Counter-friendly quote lifetimes (cdk's default 60 s melt TTL is too short
# for an in-person visit; the processor's self-test warns below comfortable
# thresholds).
[info.quote_ttl]
mint_ttl = 1800
melt_ttl = 900

[info.http_cache]
backend = "memory"
ttl = 60
tti = 60

[mint_info]
name = "{unit} mint"
description = "Ecash mint for the {unit} unit, operated with Pecan"

[database]
engine = "sqlite"

[[payment_backend]]
backend = "grpcprocessor"
unit = "{unit}"
min_mint = 1
max_mint = 500000
min_melt = 1
max_melt = 500000

[grpc_processor]
supported_units = ["{unit}"]
# Compose-internal DNS — the mint and processor share the stack's network.
address = "processor"
port = 50051
# Plaintext gRPC never leaves the compose network in this topology.
allow_insecure = true
"#
    )
}

/// Write `<install-dir>/mint/{config.toml,mnemonic}` with tight modes. The
/// seed is never overwritten — a leftover mnemonic from a previous install
/// is a hard error, not a silent replacement.
pub fn write_mint_files(
    install_dir: &Path,
    unit: &str,
    mint_url: &str,
    mnemonic: &str,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = install_dir.join("mint");
    let seed_path = dir.join("mnemonic");
    if seed_path.exists() {
        bail!(
            "{} already exists — refusing to overwrite a mint seed or configuration",
            seed_path.display()
        );
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;

    let config_path = dir.join("config.toml");
    crate::storage::atomic_write(
        &config_path,
        render_config_toml(unit, mint_url).as_bytes(),
        0o600,
    )?;

    // No trailing newline: cdk's file: resolver reads the file verbatim.
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&seed_path)
        .with_context(|| format!("create {}", seed_path.display()))?
        .write_all(mnemonic.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonic_is_twelve_valid_words() {
        let m = generate_mnemonic();
        assert_eq!(m.split_whitespace().count(), 12);
        // Round-trips through the bip39 parser (checksum holds).
        m.parse::<bip39::Mnemonic>().expect("valid mnemonic");
        // And two generations differ.
        assert_ne!(m, generate_mnemonic());
    }

    #[test]
    fn unit_slug_rule_matches_the_processor() {
        assert_eq!(validate_unit_slug(" ORA ").expect("ok"), "ora");
        assert_eq!(validate_unit_slug("x_1-2").expect("ok"), "x_1-2");
        assert!(validate_unit_slug("").is_err());
        assert!(validate_unit_slug("no spaces").is_err());
        assert!(validate_unit_slug("ümlaut").is_err());
    }

    #[test]
    fn config_toml_carries_the_unit_and_backend() {
        let toml = render_config_toml("ora", "https://mint.example.org");
        assert!(toml.contains("[[payment_backend]]"));
        assert!(toml.contains("backend = \"grpcprocessor\""));
        assert!(toml.contains("unit = \"ora\""));
        assert!(toml.contains("supported_units = [\"ora\"]"));
        assert!(toml.contains("url = \"https://mint.example.org\""));
        assert!(toml.contains("mnemonic = \"file:/run/secrets/mint-mnemonic\""));
        assert!(toml.contains("address = \"processor\""));
        assert!(!toml.contains("[[ln]]"));
    }

    #[test]
    fn mint_files_written_with_modes_and_seed_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        write_mint_files(
            dir.path(),
            "ora",
            "http://1.2.3.4:3338",
            "word ".repeat(12).trim(),
        )
        .expect("write");
        let seed_path = dir.path().join("mint/mnemonic");
        let mode = std::fs::metadata(&seed_path)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(dir.path().join("mint/config.toml").is_file());
        // A second write must refuse to touch the seed.
        assert!(write_mint_files(dir.path(), "ora", "http://1.2.3.4:3338", "other").is_err());
        assert_eq!(
            std::fs::read_to_string(&seed_path).expect("read"),
            "word ".repeat(12).trim()
        );
    }
}
