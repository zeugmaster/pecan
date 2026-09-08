//! Stage and verify updates before touching the live deployment. A consistent
//! data backup and previous deployment files survive any activation failure.

use crate::{
    caddy,
    compose::{self, Stack},
    envfile::EnvFile,
    install, ops, release, storage, ui, UpdateArgs,
};
use anyhow::{bail, Context, Result};
use std::path::Path;

const FILES: &[&str] = &[
    "docker-compose.yml",
    "Caddyfile",
    ".env.example",
    "mintctl",
    ".env",
];

pub fn run(args: &UpdateArgs) -> Result<()> {
    let stack = Stack::discover()?;
    let _lock = if args.check {
        None
    } else {
        Some(storage::lock(&stack.install_dir)?)
    };
    let current_env = EnvFile::load(&stack.env_path())?;
    if current_env.get("MINT_MODE").is_some() {
        bail!("this pre-0.2 installation contains a managed mint. Follow 'Migrating from the bundled mint' in docs/operations.md before updating; its container and data have been left in place");
    }
    let bundled = ops::has_profile(&current_env, "mint");
    if (args.with_mint || args.mint_version.is_some()) && !bundled {
        bail!("mint updates require an installation with a bundled mint; update an external mint with its own tooling");
    }
    let current = current_env
        .get("VERSION")
        .filter(|s| !s.is_empty())
        .context("installation has no VERSION pin")?;
    // An explicit mint tag on its own is a mint-only update, also offline.
    let target = match &args.version {
        Some(version) => version.clone(),
        None if args.mint_version.is_some() => current.clone(),
        None => release::resolve_latest_version()
            .context("pass --version vX.Y.Z to select a release explicitly")?,
    };
    release::validate_tag(&target)?;
    let source = install::artifact_source(
        args.artifacts_dir.clone(),
        args.artifact_ref.clone(),
        &target,
    );
    // --check uses system temp so it can inspect a read-only installation.
    let staging = if args.check {
        tempfile::tempdir()?
    } else {
        tempfile::tempdir_in(&stack.install_dir)?
    };
    let console_changed = target != current;
    if console_changed || args.with_mint {
        install::fetch_deploy_artifacts(&source, staging.path())?;
    }
    let current_mint = current_env.get("MINT_VERSION").filter(|s| !s.is_empty());
    if bundled && current_mint.is_none() {
        bail!("bundled installation has no MINT_VERSION pin; set its currently running image tag in .env before updating");
    }
    let target_mint = if args.with_mint {
        Some(tested_mint_tag(&staging.path().join(".env.example"))?)
    } else {
        args.mint_version.clone().or_else(|| current_mint.clone())
    };
    if let Some(tag) = &target_mint {
        release::validate_tag(tag)?;
    }
    let mint_changed = target_mint != current_mint;
    ui::say(format!(
        "Console: {current} → {target}{}",
        if console_changed { "" } else { " (unchanged)" }
    ));
    if let Some(tag) = &target_mint {
        ui::say(format!(
            "Mint:    {} → {tag}{}",
            current_mint.as_deref().unwrap_or("unknown"),
            if mint_changed { "" } else { " (unchanged)" }
        ));
    }
    if args.check || (!console_changed && !mint_changed) {
        return Ok(());
    }
    ui::say("A backup will be saved before updating. Services pause briefly for the snapshot.");
    if !ui::Ui::new(args.yes).confirm("Apply this update?") {
        bail!("update cancelled; for unattended updates pass --yes");
    }
    compose::ensure_docker(false)?;
    for name in FILES {
        if *name == "mintctl" && console_changed {
            install::install_binary(&source, &target, &staging.path().join(name))?;
        } else if (*name == ".env" || !console_changed) && stack.install_dir.join(name).exists() {
            std::fs::copy(stack.install_dir.join(name), staging.path().join(name))?;
        }
    }
    let mut next = EnvFile::load(&staging.path().join(".env"))?;
    next.set("VERSION", &target);
    if let Some(tag) = &target_mint {
        next.set("MINT_VERSION", tag);
    }
    next.save()?;
    if console_changed {
        caddy::apply_acme_email(staging.path(), &next.get("ACME_EMAIL").unwrap_or_default())?;
        caddy::apply_mint_site(
            staging.path(),
            bundled
                && ops::has_profile(&next, "tls")
                && next.get("MINT_DOMAIN").is_some_and(|d| !d.is_empty()),
        )?;
    }
    let mut services = Vec::new();
    if console_changed {
        services.push("processor");
    }
    if mint_changed || (args.with_mint && console_changed) {
        services.push("mintd");
    }
    if console_changed && ops::has_profile(&next, "tls") {
        services.push("caddy");
    }
    staged_compose(&stack, staging.path(), &["config", "--quiet"])?;
    if !args.no_pull {
        let mut pull = vec!["pull", "--quiet"];
        pull.extend(&services);
        staged_compose(&stack, staging.path(), &pull)?;
    } else {
        let mut images = vec!["config", "--images"];
        images.extend(&services);
        let output = stack
            .command_with_files(staging.path(), &images)?
            .output()?;
        if !output.status.success() {
            bail!("cannot resolve target images");
        }
        for image in String::from_utf8(output.stdout)?.lines() {
            if !compose::image_present(image) {
                bail!("--no-pull: target image {image} is not available locally");
            }
        }
    }
    // No live files or containers have changed up to this point.
    let history = stack.install_dir.join("updates");
    std::fs::create_dir_all(&history)?;
    let recovery = tempfile::Builder::new()
        .prefix("before-")
        .tempdir_in(&history)?
        .keep();
    std::fs::set_permissions(
        &recovery,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    for name in FILES {
        let path = stack.install_dir.join(name);
        if path.exists() {
            std::fs::copy(&path, recovery.join(name))?;
        }
    }
    let backup = recovery.join("backup.tar.gz");
    ops::backup_to(&stack, &backup)?;
    ui::say(format!(
        "Backup and previous deployment: {}",
        recovery.display()
    ));
    storage::atomic_write(&recovery.join("RECOVERY.txt"), format!(
        "Before update: console {current}, mint {}.\nTarget: console {target}, mint {}.\n\n\
         If startup fails, inspect mintctl logs and retry mintctl start.\n\
         Do not downgrade a mint against a database that a newer version may have migrated.\n\
         To restore the snapshot deliberately (discards all activity since the backup):\n\
         1. Run mintctl stop.\n\
         2. Copy docker-compose.yml, Caddyfile, .env.example, .env and mintctl from this directory into the installation (preserve permissions).\n\
         3. Run the restored mintctl restore with this directory's backup.tar.gz.\n\
         4. Run mintctl status and verify the console's Mint tab.\n",
        current_mint.as_deref().unwrap_or("external"), target_mint.as_deref().unwrap_or("external")
    ).as_bytes(), 0o600)?;
    // Restore previous files on a commit error, before any new service runs.
    let commit = (|| -> Result<()> {
        for name in FILES {
            if staging.path().join(name).exists() {
                std::fs::rename(staging.path().join(name), stack.install_dir.join(name))?;
            }
        }
        Ok(())
    })();
    if let Err(error) = commit {
        for name in FILES {
            let path = recovery.join(name);
            if path.exists() {
                use std::os::unix::fs::PermissionsExt;
                storage::atomic_write(
                    &stack.install_dir.join(name),
                    &std::fs::read(&path)?,
                    std::fs::metadata(&path)?.permissions().mode() & 0o777,
                )?;
            }
        }
        return Err(error.context("could not commit update; previous deployment files restored"));
    }
    // Only explicitly selected services are recreated; never remove orphans or
    // prune old images. They may be needed by another installation or recovery.
    let mut up = vec![
        "up",
        "-d",
        "--no-deps",
        "--pull",
        "never",
        "--force-recreate",
        "--wait",
        "--wait-timeout",
        "120",
    ];
    up.extend(&services);
    stack.compose(&up).with_context(|| format!(
        "update did not become healthy. Target pins are retained so you can retry 'mintctl start'. \
         Check 'mintctl logs'; backup and recovery instructions: {}. No database downgrade was attempted",
        recovery.display()))?;
    ui::say("Update complete; selected services are healthy.");
    Ok(())
}

fn staged_compose(stack: &Stack, files: &Path, args: &[&str]) -> Result<()> {
    let status = stack.command_with_files(files, args)?.status()?;
    if !status.success() {
        bail!(
            "docker compose {} failed; live deployment is unchanged",
            args.join(" ")
        );
    }
    Ok(())
}

fn tested_mint_tag(example: &Path) -> Result<String> {
    let text = std::fs::read_to_string(example)?;
    let tag = text.lines().find_map(|line| line.trim_start_matches('#').strip_prefix("MINT_VERSION="))
        .filter(|tag| !tag.is_empty()).context("target release does not declare a tested MINT_VERSION; use --mint-version <tag> explicitly")?;
    release::validate_tag(tag)?;
    Ok(tag.to_string())
}
