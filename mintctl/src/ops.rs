//! The operations subcommands: everything except install.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use time::macros::format_description;

use crate::compose::{self, Stack, BIN_LINK};
use crate::envfile::EnvFile;
use crate::release;
use crate::ui::{self, Ui};

fn env(stack: &Stack) -> Result<EnvFile> {
    EnvFile::load(&stack.env_path())
}

pub fn status() -> Result<()> {
    let stack = Stack::discover()?;
    let envf = env(&stack)?;
    let ui_port = envf.get("UI_PORT").unwrap_or_else(|| "9090".into());
    let version = envf.get("VERSION").unwrap_or_default();

    stack.compose(&["ps"])?;
    ui::say("");
    let health_url = format!("http://127.0.0.1:{ui_port}/healthz");
    match compose::probe_healthz(&health_url) {
        Some(running) => ui::say(format!("console: ok (version {running})")),
        None => ui::say(format!("console: not responding on 127.0.0.1:{ui_port}")),
    }
    if let Some(mint_version) = envf.get("MINT_VERSION").filter(|v| !v.is_empty()) {
        let mint_port = envf.get("MINT_PORT").unwrap_or_else(|| "3338".into());
        let mint_url = format!("http://127.0.0.1:{mint_port}/v1/info");
        if compose::wait_http_ok(&mint_url, std::time::Duration::from_secs(3)) {
            ui::say(format!("mint:    ok (cashubtc/mintd:{mint_version})"));
        } else {
            ui::say(format!(
                "mint:    not responding on 127.0.0.1:{mint_port} (cashubtc/mintd:{mint_version})"
            ));
        }
    }
    ui::say(format!(
        "installed version: {}",
        if version.is_empty() {
            "unknown"
        } else {
            &version
        }
    ));
    match release::resolve_latest_version() {
        Ok(latest) if latest != version => ui::say(format!(
            "latest release:    {latest}  → run 'mintctl update'"
        )),
        Ok(latest) => ui::say(format!("latest release:    {latest} (up to date)")),
        Err(_) => {}
    }
    Ok(())
}

pub fn logs(services: &[String]) -> Result<()> {
    let stack = Stack::discover()?;
    let mut args = vec!["logs", "-f", "--tail=200"];
    args.extend(services.iter().map(String::as_str));
    stack.compose(&args)
}

pub fn backup(output: Option<PathBuf>) -> Result<()> {
    let stack = Stack::discover()?;
    let _lock = crate::storage::lock(&stack.install_dir)?;
    let out = match output {
        Some(path) => absolutize(path)?,
        None => {
            let stamp = time::OffsetDateTime::now_utc().format(format_description!(
                "[year][month][day]-[hour][minute][second]"
            ))?;
            absolutize(PathBuf::from(format!("pecan-backup-{stamp}.tar.gz")))?
        }
    };
    backup_to(&stack, &out)?;
    let has_mint = has_profile(&env(&stack)?, "mint");
    ui::say("");
    ui::say(format!("Backup written to {}", out.display()));
    if has_mint {
        ui::say("It contains the operator accounts (password hashes), the attachment");
        ui::say("configuration, the ticket ledger, AND the mint's database and SEED —");
        ui::say("this archive can issue your ecash. Encrypt it and store it off this");
        ui::say("server.");
    } else {
        ui::say("It contains the operator accounts (password hashes), the attachment");
        ui::say("configuration, and the ticket ledger — store it encrypted, off this");
        ui::say("server. The mint's own data is NOT included; the mint is backed up");
        ui::say("by whoever operates it.");
    }
    Ok(())
}

pub(crate) fn has_profile(env: &EnvFile, profile: &str) -> bool {
    env.get("COMPOSE_PROFILES")
        .unwrap_or_default()
        .split(',')
        .any(|p| p.trim() == profile)
}

/// Stream tar to a private host-owned file: Docker never creates root-owned
/// backup files in the user's directory. Always attempt to restart after stop,
/// including when spawning tar fails. Keep an existing archive on failure.
pub(crate) fn backup_to(stack: &Stack, out: &std::path::Path) -> Result<()> {
    use std::process::{Command, Stdio};
    let envf = env(stack)?;
    if out.exists() {
        bail!(
            "backup already exists: {}; choose another filename",
            out.display()
        );
    }
    let has_mint = has_profile(&envf, "mint");
    let project = envf
        .get("COMPOSE_PROJECT_NAME")
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| compose::project_name(&stack.install_dir));
    let temp = tempfile::NamedTempFile::new_in(out.parent().context("backup path has no parent")?)?;
    // Pull the helper before downtime, and only if it is absent.
    if !compose::image_present("debian:bookworm-slim") {
        let status = Command::new("docker")
            .args(["pull", "debian:bookworm-slim"])
            .status()?;
        if !status.success() {
            bail!("could not pull the backup helper image");
        }
    }
    // Restart exactly the services that were running, including on failures.
    let running = stack
        .compose_command(&["ps", "--services", "--filter", "status=running"])?
        .output()?;
    if !running.status.success() {
        bail!("cannot determine running services before backup");
    }
    let running = String::from_utf8(running.stdout)?;
    let services: Vec<&str> = running.lines().filter(|s| !s.is_empty()).collect();
    let mut stop = vec!["stop"];
    stop.extend(&services);
    let snapshot = (|| -> Result<()> {
        if !services.is_empty() {
            ui::say("Stopping services for a consistent snapshot ...");
            stack.compose(&stop)?;
        }
        let mut cmd = Command::new("docker");
        cmd.args(["run", "--rm", "--network", "none"])
            .args(["-v", &format!("{project}_config-data:/backup/config:ro")])
            .args([
                "-v",
                &format!("{project}_processor-data:/backup/processor:ro"),
            ])
            .args([
                "-v",
                &format!("{}:/backup/install:ro", stack.install_dir.display()),
            ]);
        if has_mint {
            cmd.args(["-v", &format!("{project}_mintd-data:/backup/mint-data:ro")]);
        }
        cmd.arg("debian:bookworm-slim").args([
            "tar",
            "czf",
            "-",
            "-C",
            "/backup",
            "config",
            "processor",
            "install/.env",
        ]);
        if has_mint {
            cmd.args(["mint-data", "install/mint"]);
        }
        let status = cmd
            .stdout(Stdio::from(temp.as_file().try_clone()?))
            .status()
            .context("run backup container")?;
        if !status.success() {
            bail!("the backup container failed");
        }
        temp.as_file().sync_all()?;
        Ok(())
    })();
    let mut start = vec!["start"];
    start.extend(&services);
    let restart = if services.is_empty() {
        Ok(())
    } else {
        stack.compose(&start)
    };
    if let Err(error) = snapshot {
        return match restart {
            Ok(()) => Err(error),
            Err(restart_error) => Err(error.context(format!(
                "services also failed to restart: {restart_error:#}; run mintctl start"
            ))),
        };
    }
    // Keep the snapshot even if restarting fails; it is the recovery path.
    temp.persist_noclobber(out).with_context(|| {
        format!(
            "save backup {} (existing archives are never replaced)",
            out.display()
        )
    })?;
    restart.context("backup saved, but services could not restart; run mintctl start")?;
    Ok(())
}

pub fn restore(archive: PathBuf, yes: bool) -> Result<()> {
    let stack = Stack::discover()?;
    let _lock = crate::storage::lock(&stack.install_dir)?;
    let envf = env(&stack)?;
    let ui_prompt = Ui::new(yes);
    if !archive.is_file() {
        bail!("no such file: {}", archive.display());
    }
    let archive = absolutize(archive)?;
    let project = envf
        .get("COMPOSE_PROJECT_NAME")
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| compose::project_name(&stack.install_dir));
    let archive_dir = archive.parent().context("archive path has no directory")?;
    let archive_name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .context("archive path has no file name")?;

    // Which shape does the archive have, and does it match this install?
    let listing = std::process::Command::new("tar")
        .args(["tzf", &archive.display().to_string()])
        .output()
        .context("inspect the archive (tar tzf)")?;
    if !listing.status.success() {
        bail!("could not read {} (tar tzf failed)", archive.display());
    }
    let members = String::from_utf8_lossy(&listing.stdout);
    let archive_has_mint = members.lines().any(|l| l.starts_with("mint-data/"));
    let install_has_mint = envf
        .get("COMPOSE_PROFILES")
        .unwrap_or_default()
        .split(',')
        .any(|p| p.trim() == "mint");
    if archive_has_mint && !install_has_mint {
        bail!(
            "this archive contains a bundled mint (database + seed), but this install \
             is processor-only. Re-install with --with-mint first, then restore."
        );
    }
    if !archive_has_mint && install_has_mint {
        bail!(
            "this install bundles a mint, but the archive has no mint data — restoring \
             would leave the processor's ledger out of step with the running mint. \
             Restore it into a processor-only install, or use a bundled-mint backup."
        );
    }

    // Read these before stopping or replacing state; write on the host so
    // a non-root operator retains ownership even with a rootful daemon.
    let mut mint_files = Vec::new();
    if archive_has_mint {
        for name in ["config.toml", "mnemonic"] {
            let output = std::process::Command::new("tar")
                .arg("xOzf")
                .arg(&archive)
                .arg(format!("install/mint/{name}"))
                .output()?;
            if !output.status.success() {
                bail!("archive is missing mint/{name}");
            }
            mint_files.push((name, output.stdout));
        }
    }
    ui::say("Restoring replaces the processor's current state (attachment config,");
    ui::say(format!(
        "operator accounts, ticket ledger) of project '{project}' with the archive"
    ));
    if archive_has_mint {
        ui::say("contents, INCLUDING the mint's database and seed. (.env is not touched.)");
    } else {
        ui::say("contents. (.env is not touched; the mint is unaffected.)");
    }
    if !ui_prompt.confirm("Continue?") {
        bail!("restore cancelled");
    }
    stack.compose(&["down", "--remove-orphans"])?;
    let mut volumes = vec!["config-data", "processor-data"];
    if archive_has_mint {
        volumes.push("mintd-data");
    }
    for vol in &volumes {
        let status = std::process::Command::new("docker")
            .args(["volume", "create", &format!("{project}_{vol}")])
            .stdout(std::process::Stdio::null())
            .status()
            .context("docker volume create")?;
        if !status.success() {
            bail!("could not create volume {project}_{vol}");
        }
    }
    // Archive filenames are positional shell arguments, never shell source.
    let script = if archive_has_mint {
        "find /restore/config /restore/processor /restore/mint-data -mindepth 1 -delete && tar xzf \"$1\" -C /restore config processor mint-data"
    } else {
        "find /restore/config /restore/processor -mindepth 1 -delete && tar xzf \"$1\" -C /restore config processor"
    };
    let mut cmd = std::process::Command::new("docker");
    cmd.args(["run", "--rm"])
        .args(["-v", &format!("{project}_config-data:/restore/config")])
        .args([
            "-v",
            &format!("{project}_processor-data:/restore/processor"),
        ])
        .args(["-v", &format!("{}:/in:ro", archive_dir.display())]);
    if archive_has_mint {
        cmd.args(["-v", &format!("{project}_mintd-data:/restore/mint-data")]);
    }
    let status = cmd
        .arg("debian:bookworm-slim")
        .args([
            "sh",
            "-c",
            script,
            "restore",
            &format!("/in/{archive_name}"),
        ])
        .status()
        .context("run the restore container")?;
    if !status.success() {
        bail!("the restore container failed");
    }
    if archive_has_mint {
        let dir = stack.install_dir.join("mint");
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        for (name, content) in mint_files {
            crate::storage::atomic_write(&dir.join(name), &content, 0o600)?;
        }
    }
    stack.compose(&["up", "-d", "--remove-orphans"])?;
    ui::say("restore complete — check 'mintctl status'");
    Ok(())
}

pub fn start() -> Result<()> {
    let stack = Stack::discover()?;
    let _lock = crate::storage::lock(&stack.install_dir)?;
    stack.compose(&["up", "-d", "--wait", "--wait-timeout", "120"])
}

pub fn stop() -> Result<()> {
    let stack = Stack::discover()?;
    let _lock = crate::storage::lock(&stack.install_dir)?;
    stack.compose(&["stop"])
}

pub fn uninstall(purge: bool, yes: bool) -> Result<()> {
    let stack = Stack::discover()?;
    let _lock = crate::storage::lock(&stack.install_dir)?;
    let envf = env(&stack)?;
    let ui_prompt = Ui::new(yes);
    let project = envf
        .get("COMPOSE_PROJECT_NAME")
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| compose::project_name(&stack.install_dir));

    if purge {
        let has_mint = envf
            .get("COMPOSE_PROFILES")
            .unwrap_or_default()
            .split(',')
            .any(|p| p.trim() == "mint");
        ui::say("PURGE deletes the containers, ALL VOLUMES (operator accounts,");
        if has_mint {
            ui::say("attachment config, the ticket ledger, the mint's database and its");
            ui::say(format!(
                "SEED in {}/mint), and the directory itself. This cannot",
                stack.install_dir.display()
            ));
            ui::say("be undone — without a backup, the mint's issued ecash dies with it.");
        } else {
            ui::say(format!(
                "attachment config, the ticket ledger), and {}. This cannot be undone.",
                stack.install_dir.display()
            ));
        }
        if !ui_prompt.confirm_typed(
            &format!("Type the project name ({project}) to confirm"),
            &project,
        ) {
            bail!("confirmation did not match; nothing was deleted");
        }
        stack.compose(&["down", "-v", "--remove-orphans"])?;
        remove_bin_link(&stack);
        std::fs::remove_dir_all(&stack.install_dir)
            .with_context(|| format!("remove {}", stack.install_dir.display()))?;
        ui::say(format!(
            "purged project {project} and {}",
            stack.install_dir.display()
        ));
    } else {
        stack.compose(&["down", "--remove-orphans"])?;
        remove_bin_link(&stack);
        ui::say(format!(
            "containers removed; volumes and {} kept.",
            stack.install_dir.display()
        ));
        ui::say(format!(
            "Re-run '{}/mintctl start' to bring it back, or",
            stack.install_dir.display()
        ));
        ui::say("'mintctl uninstall --purge' to delete everything.");
    }
    Ok(())
}

pub fn version() -> Result<()> {
    let stack = Stack::discover()?;
    let envf = env(&stack)?;
    let version = envf.get("VERSION").unwrap_or_default();
    ui::say(format!(
        "installed: {}",
        if version.is_empty() {
            "unknown"
        } else {
            &version
        }
    ));
    let _ = stack.compose(&["images"]);
    if let Ok(latest) = release::resolve_latest_version() {
        ui::say(format!("latest release: {latest}"));
    }
    Ok(())
}

/// Only remove the /usr/local/bin symlink when it points into this install.
fn remove_bin_link(stack: &Stack) {
    let mut links = vec![PathBuf::from(BIN_LINK)];
    if let Ok(link) = compose::cli_link() {
        links.push(link);
    }
    for link in links {
        if std::fs::read_link(&link).ok().as_deref() == Some(&stack.install_dir.join("mintctl")) {
            let _ = std::fs::remove_file(link);
        }
    }
}

fn absolutize(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
