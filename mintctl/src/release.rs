//! Release resolution, artifact fetching, and binary asset downloads.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

pub const REPO: &str = "zeugmaster/pecan";

/// Release tag stamped by CI (`MINTCTL_VERSION=vX.Y.Z`); "dev" from a checkout.
pub fn own_version() -> &'static str {
    option_env!("MINTCTL_VERSION").unwrap_or("dev")
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .user_agent(&format!("mintctl/{}", own_version()))
        .build()
}

/// Tags are also interpolated into Compose .env files and release URLs.
pub fn validate_tag(tag: &str) -> Result<()> {
    let valid = !tag.is_empty()
        && tag.len() <= 128
        && tag.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid {
        bail!(
            "invalid image/release tag {tag:?}; use letters, digits, dots, underscores and hyphens"
        );
    }
    Ok(())
}

fn get(url: &str) -> Result<ureq::Response> {
    for attempt in 0..3 {
        match agent().get(url).call() {
            Ok(response) => return Ok(response),
            Err(error) => {
                let retry = matches!(
                    &error,
                    ureq::Error::Transport(_) | ureq::Error::Status(408 | 429 | 500..=599, _)
                );
                if !retry || attempt == 2 {
                    return Err(error.into());
                }
                std::thread::sleep(Duration::from_secs(1 << attempt));
            }
        }
    }
    unreachable!()
}

/// Latest release tag: GitHub API first, then the /releases/latest redirect
/// (which carries the tag in its final URL) when the API is rate-limited.
pub fn resolve_latest_version() -> Result<String> {
    let api = format!("https://api.github.com/repos/{REPO}/releases/latest");
    if let Ok(resp) = agent().get(&api).timeout(Duration::from_secs(15)).call() {
        if let Ok(body) = resp.into_json::<serde_json::Value>() {
            if let Some(tag) = body.get("tag_name").and_then(|t| t.as_str()) {
                if !tag.is_empty() {
                    return Ok(tag.to_string());
                }
            }
        }
    }
    let human = format!("https://github.com/{REPO}/releases/latest");
    let resp = agent()
        .get(&human)
        .timeout(Duration::from_secs(15))
        .call()
        .context("could not reach github.com to resolve the latest release")?;
    let final_url = resp.get_url().to_string();
    final_url
        .split("/releases/tag/")
        .nth(1)
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.to_string())
        .ok_or_else(|| anyhow!("could not resolve the latest release from GitHub. Pass --version vX.Y.Z explicitly."))
}

/// Where install/update take their deployment artifacts from.
#[derive(Clone)]
pub enum ArtifactSource {
    /// A git ref (normally the release tag) on raw.githubusercontent.com.
    Ref(String),
    /// A local checkout (`--artifacts-dir`), for offline testing.
    Dir(PathBuf),
}

impl ArtifactSource {
    pub fn fetch(&self, rel: &str, dest: &Path) -> Result<()> {
        match self {
            ArtifactSource::Dir(dir) => {
                std::fs::copy(dir.join(rel), dest)
                    .with_context(|| format!("copy {} from {}", rel, dir.display()))?;
                Ok(())
            }
            ArtifactSource::Ref(git_ref) => {
                let url = format!("https://raw.githubusercontent.com/{REPO}/{git_ref}/{rel}");
                let resp =
                    get(&url).with_context(|| format!("could not download {rel} from {url}"))?;
                let mut reader = resp.into_reader();
                let mut file = std::fs::File::create(dest)
                    .with_context(|| format!("create {}", dest.display()))?;
                std::io::copy(&mut reader, &mut file)
                    .with_context(|| format!("write {}", dest.display()))?;
                Ok(())
            }
        }
    }
}

/// Release-asset name for this platform, e.g. `mintctl-linux-amd64`.
pub fn binary_asset_name() -> Result<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        other => bail!("no mintctl binaries are published for {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => bail!("no mintctl binaries are published for {other}"),
    };
    Ok(format!("mintctl-{os}-{arch}"))
}

/// Download the pinned release's mintctl binary for this platform and verify
/// it against the release's SHA256SUMS. Returns the temp path (same dir as
/// `near`, so a later rename is atomic).
pub fn download_release_binary(version: &str, near: &Path) -> Result<PathBuf> {
    validate_tag(version)?;
    let asset = binary_asset_name()?;
    let base = format!("https://github.com/{REPO}/releases/download/{version}");

    let sums = get(&format!("{base}/SHA256SUMS"))
        .with_context(|| format!("download SHA256SUMS for {version}"))?
        .into_string()?;
    let expected = sums
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let digest = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (name == asset).then(|| digest.to_ascii_lowercase())
        })
        .ok_or_else(|| anyhow!("{asset} is not listed in the {version} SHA256SUMS"))?;

    let dir = near.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".mintctl-download-")
        .tempfile_in(dir)
        .context("create download temp file")?;
    let resp = get(&format!("{base}/{asset}"))
        .with_context(|| format!("download {asset} for {version}"))?;
    std::io::copy(&mut resp.into_reader(), &mut tmp).with_context(|| format!("write {asset}"))?;

    let bytes = std::fs::read(tmp.path())?;
    let actual = hex(&Sha256::digest(&bytes));
    if actual != expected {
        bail!("checksum mismatch for {asset}: expected {expected}, got {actual}");
    }
    let (_file, path) = tmp.keep().context("persist downloaded binary")?;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    Ok(path)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_matches_this_platform() {
        let name = binary_asset_name().expect("supported platform");
        assert!(name.starts_with("mintctl-"));
        assert!(name.contains("amd64") || name.contains("arm64"));
    }

    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex(&[0xde, 0xad, 0x01]), "dead01");
    }
}
