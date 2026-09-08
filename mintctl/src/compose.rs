//! Docker / docker compose plumbing and the installed-stack context.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};

use crate::envfile::EnvFile;
use crate::ui;

pub const BIN_LINK: &str = "/usr/local/bin/mintctl";
pub const DEFAULT_LINUX_DIR: &str = "/opt/pecan";

pub fn cli_link() -> Result<PathBuf> {
    if is_root() {
        Ok(PathBuf::from(BIN_LINK))
    } else {
        Ok(
            PathBuf::from(std::env::var("HOME").context("HOME is not set")?)
                .join(".local/bin/mintctl"),
        )
    }
}

/// An existing installation (subcommand context): the directory holding
/// docker-compose.yml, .env, and the mintctl binary itself.
pub struct Stack {
    pub install_dir: PathBuf,
}

impl Stack {
    /// Resolve like the bash `require_install_dir`: MINTCTL_DIR wins, else the
    /// directory this binary lives in (symlinks resolved); a .env must exist.
    pub fn discover() -> Result<Self> {
        let install_dir = if let Ok(dir) = std::env::var("MINTCTL_DIR") {
            PathBuf::from(dir)
        } else {
            let exe = std::env::current_exe().context("resolve this binary's path")?;
            let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
            exe.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        };
        if !install_dir.join(".env").is_file() {
            bail!(
                "no installation found next to this binary ({}). \
                 Set MINTCTL_DIR=/path/to/install or run the installer first.",
                install_dir.display()
            );
        }
        let install_dir =
            std::fs::canonicalize(install_dir).context("resolve install directory")?;
        Ok(Self { install_dir })
    }

    pub fn env_path(&self) -> PathBuf {
        self.install_dir.join(".env")
    }

    /// `docker compose` pinned to this install's compose file and project dir
    /// (never auto-loads overrides), streaming output to the terminal.
    pub fn compose(&self, args: &[&str]) -> Result<()> {
        let status = self
            .compose_command(args)?
            .status()
            .context("run docker compose")?;
        if !status.success() {
            bail!("docker compose {} failed", args.join(" "));
        }
        Ok(())
    }

    /// Buffered variant for the wizard: nothing prints unless it fails, in
    /// which case the tail of the combined output rides along in the error.
    pub fn compose_quiet(&self, args: &[&str]) -> Result<()> {
        let output = self
            .compose_command(args)?
            .output()
            .context("run docker compose")?;
        if output.status.success() {
            return Ok(());
        }
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        let tail: Vec<&str> = combined
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        bail!(
            "docker compose {} failed:\n{}",
            args.join(" "),
            tail.join("\n")
        );
    }

    pub fn compose_command(&self, args: &[&str]) -> Result<Command> {
        self.command_with_files(&self.install_dir, args)
    }

    /// Stage artifacts elsewhere while resolving mounts and volumes against
    /// the original project. The install's .env wins over the caller's shell.
    pub fn command_with_files(&self, files: &Path, args: &[&str]) -> Result<Command> {
        let env = EnvFile::load(&files.join(".env"))?;
        let mut cmd = Command::new("docker");
        cmd.arg("compose")
            .arg("-f")
            .arg(files.join("docker-compose.yml"))
            .arg("--env-file")
            .arg(files.join(".env"))
            .arg("--project-directory")
            .arg(&self.install_dir)
            .arg("--project-name")
            .arg(
                env.get("COMPOSE_PROJECT_NAME")
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| project_name(&self.install_dir)),
            )
            .current_dir(&self.install_dir)
            .env_remove("COMPOSE_PROFILES")
            .env_remove("COMPOSE_FILE")
            .args(args);
        for key in env.keys() {
            cmd.env_remove(key);
        }
        Ok(cmd)
    }
}

/// Compose project name derived from the install dir (bash `project_name`).
pub fn project_name(install_dir: &Path) -> String {
    let base = install_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "pecan".into());
    let name: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_start_matches(['-', '_']);
    if name.is_empty() {
        "pecan".into()
    } else {
        name.into()
    }
}

/// New installations include a path suffix: two users can both install into
/// ~/pecan on a shared daemon without sharing named volumes. Existing projects
/// retain the COMPOSE_PROJECT_NAME already recorded in their .env.
pub fn install_project_name(install_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(install_dir.as_os_str().as_encoded_bytes());
    format!(
        "{}-{:02x}{:02x}{:02x}{:02x}",
        project_name(install_dir),
        digest[0],
        digest[1],
        digest[2],
        digest[3]
    )
}

/// Whether an image reference exists in the local daemon (docker image
/// inspect). Used to fail fast when --no-pull names an image that was never
/// built or tagged locally.
pub fn image_present(reference: &str) -> bool {
    Command::new("docker")
        .args(["image", "inspect", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn docker_available() -> bool {
    Command::new("docker")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn docker_daemon_running() -> bool {
    Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn compose_v2_available() -> bool {
    Command::new("docker")
        .args(["compose", "version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn is_root() -> bool {
    // Effective uid without a libc dependency: id -u is POSIX.
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
        .unwrap_or(false)
}

/// Headless docker preflight. Piping get.docker.com into a root shell is
/// never implicit: it requires the explicit --install-docker consent flag
/// (the guided wizard asks interactively instead).
pub fn ensure_docker(allow_install: bool) -> Result<()> {
    if !docker_available() {
        if cfg!(target_os = "macos") {
            bail!(
                "Docker is not installed. Install Docker Desktop from \
                 https://docs.docker.com/desktop/ (or OrbStack) and re-run."
            );
        }
        if allow_install {
            if !is_root() {
                bail!("--install-docker installs system Docker and needs root. Install rootless Docker first (https://docs.docker.com/engine/security/rootless/), or ask an administrator to install Docker and grant this user access, then re-run mintctl as this user.");
            }
            ui::say("Docker is not installed — installing via https://get.docker.com ...");
            let installer = tempfile::NamedTempFile::new()?;
            let download = Command::new("curl")
                .args([
                    "-fsSL",
                    "--retry",
                    "3",
                    "--connect-timeout",
                    "15",
                    "--max-time",
                    "120",
                    "https://get.docker.com",
                    "-o",
                ])
                .arg(installer.path())
                .status()
                .context("download Docker installer")?;
            if !download.success() {
                bail!("could not download Docker installer; nothing was executed");
            }
            let status = Command::new("sh")
                .arg(installer.path())
                .status()
                .context("run the Docker convenience installer")?;
            if !status.success() {
                bail!("the Docker installer failed");
            }
        } else {
            bail!(
                "Docker is not installed. Re-run with --install-docker to fetch it from \
                 https://get.docker.com, or install Docker yourself and re-run."
            );
        }
    }
    if !docker_daemon_running() {
        if !is_root() && !cfg!(target_os = "macos") {
            bail!(
                "cannot talk to the Docker daemon as this user. Start your rootless Docker daemon \
                 or ask an administrator for Docker access (then log in again). Check 'docker context show' and 'docker info'."
            );
        }
        bail!("the Docker daemon is not responding. Is it running?");
    }
    if !compose_v2_available() {
        bail!(
            "Docker Compose v2 is required (the 'docker compose' plugin). \
             Install docker-compose-plugin and re-run."
        );
    }
    let help = Command::new("docker")
        .args(["compose", "up", "--help"])
        .output()?;
    if !help.status.success() || !String::from_utf8_lossy(&help.stdout).contains("--wait-timeout") {
        bail!("Docker Compose is too old: upgrade the Compose v2 plugin to a version supporting 'up --wait --wait-timeout' before continuing");
    }
    Ok(())
}

/// Poll the processor's /healthz until it answers (bash `wait_healthy`).
pub fn wait_healthy(ui_port: u16, budget: Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    let url = format!("http://127.0.0.1:{ui_port}/healthz");
    while std::time::Instant::now() < deadline {
        if probe_healthz(&url).is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    false
}

/// Poll an arbitrary URL until it answers 2xx (the bundled mint's /v1/info).
pub fn wait_http_ok(url: &str, budget: Duration) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();
    let deadline = std::time::Instant::now() + budget;
    while std::time::Instant::now() < deadline {
        if agent.get(url).call().is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    false
}

/// GET /healthz and return the reported build version, if any.
pub fn probe_healthz(url: &str) -> Option<String> {
    let resp = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build()
        .get(url)
        .call()
        .ok()?;
    let body: serde_json::Value = resp.into_json().ok()?;
    Some(
        body.get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    )
}

pub fn detect_public_ip() -> Option<String> {
    // IPv4-only endpoints first: the DNS preflight compares this address
    // against A records, and on a dual-stack host the generic endpoints
    // answer over IPv6 — which then never matches. ifconfig.me serves HTML
    // to non-curl user agents — use the /ip endpoint — and refuse anything
    // that does not parse as an address.
    for url in [
        "https://ipv4.icanhazip.com",
        "https://api.ipify.org",
        "https://ifconfig.me/ip",
        "https://icanhazip.com",
    ] {
        let Ok(resp) = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build()
            .get(url)
            .call()
        else {
            continue;
        };
        if let Ok(body) = resp.into_string() {
            let ip = body.trim();
            if ip.parse::<std::net::IpAddr>().is_ok() {
                return Some(ip.to_string());
            }
        }
    }
    None
}

/// The server's public IPv6, when it has working IPv6 egress. Used only to
/// render the optional AAAA line in the DNS note and to sanity-check
/// existing AAAA records — quick timeouts, since v4-only boxes fail here.
pub fn detect_public_ipv6() -> Option<String> {
    for url in ["https://ipv6.icanhazip.com", "https://api6.ipify.org"] {
        let Ok(resp) = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(3))
            .build()
            .get(url)
            .call()
        else {
            continue;
        };
        if let Ok(body) = resp.into_string() {
            let ip = body.trim();
            if ip.parse::<std::net::Ipv6Addr>().is_ok() {
                return Some(ip.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name_sanitizes_like_bash() {
        assert_eq!(project_name(Path::new("/opt/pecan")), "pecan");
        assert_eq!(project_name(Path::new("/srv/My Mint!")), "my-mint-");
        assert_eq!(
            project_name(Path::new("/x/under_score.dot")),
            "under_score-dot"
        );
    }
}
