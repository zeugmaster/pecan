//! `mintctl install` — plan construction and execution.
//!
//! Two front ends produce the same `InstallPlan`: the guided wizard (a TTY
//! and no `--yes`) and the pure-flag path (automation, `curl | bash --yes`,
//! CI). Execution is shared; the wizard wraps it in progress UI.
//!
//! Two shapes: processor only (attach a mint you already run — in the
//! console's Mint tab, or headlessly via --unit/--mint-url), or processor +
//! a bundled cdk-mintd (--with-mint) that boots fully connected.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use time::format_description::well_known::Rfc3339;

use crate::caddy;
use crate::compose::{self, Stack, DEFAULT_LINUX_DIR};
use crate::dns;
use crate::mint;
use crate::passphrase;
use crate::preflight::{self, PortStatus};
use crate::release::{self, ArtifactSource};
use crate::ui;
use crate::wizard;
use crate::InstallArgs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Bundled Caddy terminates TLS for the console domain.
    DomainTls,
    /// No proxy; the console port is exposed directly (LAN / testing).
    PlainHttp,
    /// The operator's own reverse proxy terminates TLS; we bind loopback
    /// and hand them a ready-made proxy snippet.
    BehindProxy,
}

/// The console's Mint-tab attachment pre-seeded at first boot (bundled
/// installs and headless --unit/--mint-url attaches). Lands in .env as the
/// INITIAL_* keys the processor consumes only while no setup.json exists.
#[derive(Clone)]
pub struct AttachPlan {
    pub unit: String,
    pub mint_url: String,
    /// host[:port] the mint dials to reach this processor.
    pub advertised_grpc: String,
}

/// A bundled mint (compose profile "mint", official cashubtc/mintd image).
#[derive(Clone)]
pub struct MintPlan {
    /// Public hostname (empty in plain-HTTP mode).
    pub mint_domain: String,
    /// The wallet-facing URL — also the [info].url in the mint's config and
    /// the processor's attached mint URL.
    pub mint_url: String,
    /// Host port for the mint's HTTP API.
    pub mint_port: u16,
    /// 127.0.0.1 behind Caddy/own proxy, 0.0.0.0 in plain-HTTP mode.
    pub mint_bind_addr: String,
    /// cashubtc/mintd image tag (pinned independently of the pecan VERSION).
    pub mintd_version: String,
    /// The mint's BIP39 seed — generated at plan time, written 0600, shown
    /// once in the finish screen.
    pub mnemonic: String,
}

#[derive(Clone)]
pub struct InstallPlan {
    pub install_dir: PathBuf,
    pub version: String,
    pub no_pull: bool,
    pub access: AccessMode,
    pub console_domain: String,
    pub acme_email: String,
    pub ui_port: u16,
    pub bind_addr: String,
    /// Where the payment gRPC is published for the operator's mintd: loopback
    /// for a same-host (or bundled) mintd, 0.0.0.0 (plus operator
    /// firewalling or TLS) for one on another machine.
    pub grpc_bind_addr: String,
    pub grpc_port: u16,
    pub public_ip: Option<String>,
    pub admin_password: String,
    /// First-boot attachment (both shapes; None = attach later in the console).
    pub attach: Option<AttachPlan>,
    /// The bundled mint (None = processor only).
    pub mint: Option<MintPlan>,
}

impl InstallPlan {
    pub fn compose_profiles(&self) -> String {
        let mut profiles: Vec<&str> = Vec::new();
        if self.access == AccessMode::DomainTls {
            profiles.push("tls");
        }
        if self.mint.is_some() {
            profiles.push("mint");
        }
        profiles.join(",")
    }

    /// Whether the Caddyfile carries the mint site block ({$MINT_DOMAIN}).
    pub fn mint_site_enabled(&self) -> bool {
        self.access == AccessMode::DomainTls
            && self
                .mint
                .as_ref()
                .is_some_and(|m| !m.mint_domain.is_empty())
    }

    pub fn console_url(&self) -> String {
        match self.access {
            AccessMode::DomainTls | AccessMode::BehindProxy => {
                format!("https://{}", self.console_domain)
            }
            AccessMode::PlainHttp => format!(
                "http://{}:{}",
                self.public_ip.as_deref().unwrap_or("<server>"),
                self.ui_port
            ),
        }
    }

    /// The endpoint the operator's mintd connects to, as bound on this host.
    pub fn grpc_endpoint(&self) -> String {
        format!("{}:{}", self.grpc_bind_addr, self.grpc_port)
    }
}

pub fn run(args: &InstallArgs) -> Result<()> {
    if args.yes || !ui::have_tty() {
        run_noninteractive(args)
    } else {
        wizard::run(args)
    }
}

fn run_noninteractive(args: &InstallArgs) -> Result<()> {
    preflight_platform()?;
    let install_dir = resolve_install_dir(args.dir.clone())?;
    compose::ensure_docker(args.install_docker)?;

    let version = match &args.version {
        Some(v) => v.clone(),
        None => {
            ui::say("Resolving the latest release ...");
            release::resolve_latest_version()?
        }
    };
    let source = artifact_source(
        args.artifacts_dir.clone(),
        args.artifact_ref.clone(),
        &version,
    );

    let plan = plan_from_flags(args, install_dir, version)?;
    if plan.no_pull {
        // Before anything is written: a missing local image should not
        // leave a half-finished install behind.
        ensure_local_image(&plan.version)?;
    }
    guard_fresh_dir(&plan.install_dir)?;
    let _lock = crate::storage::lock(&plan.install_dir)?;
    warn_on_unconfirmed_dns(&plan);

    ui::say(format!(
        "Installing the branch processor {}{} into {}",
        plan.version,
        if plan.mint.is_some() { " + mint" } else { "" },
        plan.install_dir.display()
    ));
    prepare_install(&source, &plan)?;

    let stack = Stack {
        install_dir: plan.install_dir.clone(),
    };
    ui::say("");
    if plan.no_pull {
        ui::say(format!(
            "Skipping the image pull (--no-pull); using the local {} image.",
            plan.version
        ));
    } else {
        ui::say(format!("Pulling the images ({}) ...", plan.version));
        pull_image(&stack)?;
    }
    if plan.mint.is_some() {
        ui::say("Validating the mint configuration ...");
        validate_mint_config(&stack)?;
    }
    stack.compose(&[
        "up",
        "-d",
        "--pull",
        "never",
        "--wait",
        "--wait-timeout",
        "120",
    ])?;

    print_summary(&plan);
    Ok(())
}

/// Run cdk-mintd's own `config validate` against the rendered import
/// document — template or secret errors surface with upstream's message
/// before anything starts.
pub fn validate_mint_config(stack: &Stack) -> Result<()> {
    stack.compose_quiet(&[
        "run",
        "--rm",
        "--no-deps",
        "mintd",
        "cdk-mintd",
        "--work-dir",
        "/data",
        "config",
        "validate",
        "--file",
        "/config/mint.toml",
    ])
}

/// Headless DNS posture: warn, never fail — Caddy retries certificates in
/// the background, and CI installs have no DNS at all.
fn warn_on_unconfirmed_dns(plan: &InstallPlan) {
    if plan.access != AccessMode::DomainTls {
        return;
    }
    let mut domains = vec![plan.console_domain.as_str()];
    if let Some(m) = &plan.mint {
        if !m.mint_domain.is_empty() {
            domains.push(m.mint_domain.as_str());
        }
    }
    let public_ipv6 = compose::detect_public_ipv6();
    for domain in domains {
        let confirmed = dns::check(domain, plan.public_ip.as_deref())
            .map(|c| c.matches)
            .unwrap_or(false);
        if !confirmed {
            ui::warn(format!(
                "DNS for {domain} does not resolve to this server yet — certificates \
                 will be retried in the background once the record is right"
            ));
        }
        if let Some(advisory) = dns::aaaa_advisory(domain, public_ipv6.as_deref()) {
            ui::warn(advisory);
        }
    }
}

/// Resolve the full plan from flags alone (no prompts). Port conflicts are
/// fatal here — headless installs must fail fast with the remedy flag named,
/// not later at `docker compose up`.
fn plan_from_flags(
    args: &InstallArgs,
    install_dir: PathBuf,
    version: String,
) -> Result<InstallPlan> {
    let console_domain = args.console_domain.clone().unwrap_or_default();
    if !console_domain.is_empty() && !dns::valid_hostname(&console_domain) {
        bail!(
            "--console-domain {console_domain} is not a valid hostname \
             (lowercase labels, dots, no scheme)"
        );
    }
    let access = if args.behind_proxy {
        if console_domain.is_empty() {
            bail!("--behind-proxy needs --console-domain: your proxy's public hostname for the console");
        }
        AccessMode::BehindProxy
    } else if args.plain_http || console_domain.is_empty() {
        AccessMode::PlainHttp
    } else {
        AccessMode::DomainTls
    };
    let public_ip = compose::detect_public_ip();

    // --- the mint: bundled, pre-attached, or neither ------------------------
    let unit = args
        .unit
        .as_deref()
        .map(mint::validate_unit_slug)
        .transpose()?;
    let (attach, mint_plan) = if args.with_mint {
        let unit = unit.ok_or_else(|| {
            anyhow::anyhow!("--with-mint needs --unit: the currency unit this install serves")
        })?;
        let mint_domain = args.mint_domain.clone().unwrap_or_default();
        if access != AccessMode::PlainHttp {
            if mint_domain.is_empty() {
                bail!("--with-mint needs --mint-domain: the mint's public hostname wallets connect to");
            }
            if !dns::valid_hostname(&mint_domain) {
                bail!("--mint-domain {mint_domain} is not a valid hostname");
            }
            if mint_domain == console_domain {
                bail!("--mint-domain must differ from --console-domain — they are two sites");
            }
        }
        let mint_port = args.mint_port.unwrap_or(3338);
        let mint_url = match &args.mint_url {
            Some(url) => url.trim_end_matches('/').to_string(),
            None => match access {
                AccessMode::DomainTls | AccessMode::BehindProxy => {
                    format!("https://{mint_domain}")
                }
                AccessMode::PlainHttp => match &public_ip {
                    Some(ip) => format!("http://{ip}:{mint_port}"),
                    None => bail!(
                        "could not detect this server's public IP for the mint URL — \
                         pass --mint-url http://<address>:{mint_port}"
                    ),
                },
            },
        };
        (
            Some(AttachPlan {
                unit,
                mint_url: mint_url.clone(),
                advertised_grpc: "processor:50051".into(),
            }),
            Some(MintPlan {
                mint_domain: if access == AccessMode::PlainHttp {
                    String::new()
                } else {
                    mint_domain
                },
                mint_url,
                mint_port,
                mint_bind_addr: match access {
                    AccessMode::PlainHttp => "0.0.0.0".into(),
                    AccessMode::DomainTls | AccessMode::BehindProxy => "127.0.0.1".into(),
                },
                mintd_version: args
                    .mint_version
                    .clone()
                    .unwrap_or_else(|| mint::MINTD_DEFAULT_TAG.into()),
                mnemonic: mint::generate_mnemonic(),
            }),
        )
    } else if let Some(unit) = unit {
        // Pre-attach an existing mint headlessly.
        let mint_url = args.mint_url.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "--unit without --with-mint needs --mint-url: the existing mint's public URL"
            )
        })?;
        let grpc_bind = args.grpc_bind.as_deref().unwrap_or("127.0.0.1");
        let advertised = match &args.advertised_grpc {
            Some(endpoint) => endpoint.clone(),
            None if grpc_bind == "127.0.0.1" => format!("127.0.0.1:{}", args.grpc_port),
            None => match &public_ip {
                Some(ip) => format!("{ip}:{}", args.grpc_port),
                None => bail!(
                    "could not detect this server's public IP for the advertised gRPC \
                     endpoint — pass --advertised-grpc <host:port>"
                ),
            },
        };
        (
            Some(AttachPlan {
                unit,
                mint_url: mint_url.trim_end_matches('/').to_string(),
                advertised_grpc: advertised,
            }),
            None,
        )
    } else {
        if args.mint_url.is_some() {
            bail!("--mint-url needs --unit: attaching a mint requires both");
        }
        (None, None)
    };

    // --- fail fast on busy ports -------------------------------------------
    let mut port_checks = vec![
        (args.ui_port, "console", "--ui-port"),
        (args.grpc_port, "payment gRPC", "--grpc-port"),
    ];
    if let Some(m) = &mint_plan {
        port_checks.push((m.mint_port, "mint", "--mint-port"));
    }
    for (port, label, flag) in port_checks {
        if preflight::port_status(port) == PortStatus::Busy {
            bail!("port {port} (for the {label}) is already in use — pass {flag} <port>");
        }
    }
    if access == AccessMode::DomainTls && preflight::ports_80_443_busy() {
        bail!(
            "ports 80/443 are already in use (an existing reverse proxy, most likely) — \
             re-run with --behind-proxy, or free the ports for the bundled Caddy"
        );
    }

    Ok(InstallPlan {
        bind_addr: args.bind.clone().unwrap_or_else(|| {
            match access {
                AccessMode::PlainHttp => "0.0.0.0",
                AccessMode::DomainTls | AccessMode::BehindProxy => "127.0.0.1",
            }
            .to_string()
        }),
        grpc_bind_addr: if mint_plan.is_some() {
            // The bundled mint dials processor:50051 over the compose
            // network; the host publish stays loopback-only.
            "127.0.0.1".to_string()
        } else {
            args.grpc_bind
                .clone()
                .unwrap_or_else(|| "127.0.0.1".to_string())
        },
        grpc_port: args.grpc_port,
        install_dir,
        version,
        no_pull: args.no_pull,
        access,
        console_domain: if access == AccessMode::PlainHttp {
            String::new()
        } else {
            console_domain
        },
        acme_email: args.email.clone().unwrap_or_default(),
        ui_port: args.ui_port,
        public_ip,
        admin_password: passphrase::generate(),
        attach,
        mint: mint_plan,
    })
}

// ---------------------------------------------------------------------------
// shared primitives (used by both front ends)
// ---------------------------------------------------------------------------

pub fn preflight_platform() -> Result<()> {
    match std::env::consts::ARCH {
        "x86_64" | "aarch64" => {}
        other => {
            bail!("unsupported architecture {other}; images are published for amd64 and arm64")
        }
    }
    match std::env::consts::OS {
        "linux" => {}
        "macos" => {
            ui::warn("macOS detected — fine for development and testing, not for production.")
        }
        other => bail!("unsupported platform: {other}"),
    }
    Ok(())
}

pub fn resolve_install_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let dir = match explicit {
        Some(dir) => dir,
        None => default_install_dir(
            compose::is_root() && cfg!(target_os = "linux"),
            std::env::var_os("HOME").as_deref(),
        )?,
    };
    if dir.as_os_str().is_empty() {
        bail!("--dir cannot be empty");
    }
    let dir = if dir.is_absolute() {
        dir
    } else {
        std::env::current_dir()?.join(dir)
    };
    Ok(std::fs::canonicalize(&dir).unwrap_or(dir))
}

fn default_install_dir(system: bool, user_home: Option<&std::ffi::OsStr>) -> Result<PathBuf> {
    if system {
        return Ok(PathBuf::from(DEFAULT_LINUX_DIR));
    }
    let path = PathBuf::from(user_home.context("HOME is not set; pass --dir")?);
    if !path.is_absolute() {
        bail!("HOME must be an absolute path; pass --dir");
    }
    Ok(path.join("pecan"))
}

pub fn guard_fresh_dir(install_dir: &Path) -> Result<()> {
    if install_dir.join(".env").exists() {
        bail!(
            "an installation already exists in {dir} — use '{dir}/mintctl start' to resume a failed install, \
             or '{dir}/mintctl update' to update. Credentials are in .env; a bundled mint's seed is in mint/mnemonic. \
             For a second instance, pass --dir and different ports.",
            dir = install_dir.display()
        );
    }
    if install_dir.join("mint").exists() {
        bail!(
            "{} contains mint data from an earlier install; recover it before reinstalling",
            install_dir.display()
        );
    }
    std::fs::create_dir_all(install_dir).with_context(|| {
        format!(
            "cannot create {} — pass --dir somewhere writable by this user",
            install_dir.display()
        )
    })?;
    tempfile::NamedTempFile::new_in(install_dir)
        .with_context(|| format!("{} is not writable by this user", install_dir.display()))?;
    Ok(())
}

/// Downloads finish before credentials are committed. After configuration is
/// written, failures can be resumed with mintctl start without changing seeds.
pub fn prepare_install(source: &ArtifactSource, plan: &InstallPlan) -> Result<()> {
    // Recheck under the operation lock, after any concurrent installer exits.
    guard_fresh_dir(&plan.install_dir)?;
    release::validate_tag(&plan.version)?;
    let mut ports = vec![plan.ui_port, plan.grpc_port];
    if let Some(mint) = &plan.mint {
        ports.push(mint.mint_port);
    }
    if plan.access == AccessMode::DomainTls {
        ports.extend([80, 443]);
    }
    if ports.contains(&0) {
        bail!("host ports must be between 1 and 65535");
    }
    let mut unique = ports.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != ports.len() {
        bail!("each service needs a distinct host port");
    }
    preflight::check_rootless_ports(&ports)?;
    if let Some(mint) = &plan.mint {
        release::validate_tag(&mint.mintd_version)?;
    }
    let staging = tempfile::tempdir_in(&plan.install_dir)?;
    fetch_deploy_artifacts(source, staging.path())?;
    install_binary(source, &plan.version, &staging.path().join("mintctl"))?;
    let mut staged_plan = plan.clone();
    staged_plan.install_dir = staging.path().to_path_buf();
    write_config(&staged_plan)?;
    let mut env = crate::envfile::EnvFile::load(&staging.path().join(".env"))?;
    env.set(
        "COMPOSE_PROJECT_NAME",
        &compose::install_project_name(&plan.install_dir),
    );
    env.save()?;
    let stack = Stack {
        install_dir: plan.install_dir.clone(),
    };
    let status = stack
        .command_with_files(staging.path(), &["config", "--quiet"])?
        .status()?;
    if !status.success() {
        bail!("invalid deployment configuration; installation was not committed");
    }
    for name in [
        "docker-compose.yml",
        "Caddyfile",
        ".env.example",
        "mintctl",
        "mint",
        "proxy-snippets",
        ".env",
    ] {
        if staging.path().join(name).exists() {
            std::fs::rename(staging.path().join(name), plan.install_dir.join(name))?;
        }
    }
    install_cli_symlink(&plan.install_dir);
    Ok(())
}

pub fn artifact_source(
    artifacts_dir: Option<PathBuf>,
    artifact_ref: Option<String>,
    version: &str,
) -> ArtifactSource {
    match artifacts_dir {
        Some(dir) => ArtifactSource::Dir(dir),
        None => ArtifactSource::Ref(artifact_ref.unwrap_or_else(|| version.to_string())),
    }
}

/// Fetch docker-compose.yml / Caddyfile / .env.example with the same sanity
/// checks the bash installer ran.
pub fn fetch_deploy_artifacts(source: &ArtifactSource, dest_dir: &Path) -> Result<()> {
    let compose_path = dest_dir.join("docker-compose.yml");
    source.fetch("docker-compose.yml", &compose_path)?;
    let compose_text = std::fs::read_to_string(&compose_path).unwrap_or_default();
    if !compose_text.lines().any(|l| l.starts_with("services:")) {
        bail!("downloaded docker-compose.yml looks wrong; aborting");
    }
    source.fetch("Caddyfile", &dest_dir.join("Caddyfile"))?;
    source.fetch(".env.example", &dest_dir.join(".env.example"))?;
    Ok(())
}

/// Put the pinned release's mintctl at `dest`. Self-copy when we ARE that
/// release (or in offline test rigs); otherwise download + verify the asset,
/// failing closed if the target binary cannot be downloaded or verified.
pub fn install_binary(source: &ArtifactSource, version: &str, dest: &Path) -> Result<()> {
    let own = std::env::current_exe().context("resolve this binary's path")?;
    let self_copy = release::own_version() == version || matches!(source, ArtifactSource::Dir(_));
    if !self_copy {
        let tmp = release::download_release_binary(version, dest)?;
        std::fs::rename(&tmp, dest)
            .with_context(|| format!("install mintctl at {}", dest.display()))?;
        return Ok(());
    }
    if std::fs::canonicalize(&own).ok() != std::fs::canonicalize(dest).ok() {
        std::fs::copy(&own, dest).with_context(|| format!("copy mintctl to {}", dest.display()))?;
    }
    std::fs::set_permissions(dest, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    Ok(())
}

/// Write .env (0600), the mint's config + seed when bundled, and finish the
/// Caddyfile / proxy snippets for the plan.
pub fn write_config(plan: &InstallPlan) -> Result<()> {
    write_env(plan)?;
    if let Some(mint_plan) = &plan.mint {
        let unit = plan
            .attach
            .as_ref()
            .map(|a| a.unit.as_str())
            .unwrap_or_default();
        mint::write_mint_files(
            &plan.install_dir,
            unit,
            &mint_plan.mint_url,
            &mint_plan.mnemonic,
        )?;
    }
    caddy::apply_acme_email(&plan.install_dir, &plan.acme_email)?;
    caddy::apply_mint_site(&plan.install_dir, plan.mint_site_enabled())?;
    if plan.access == AccessMode::BehindProxy {
        caddy::write_proxy_snippets(plan)?;
    }
    Ok(())
}

/// --no-pull promises the processor image is already in the local daemon —
/// verify that promise up front instead of failing five steps later with
/// compose's registry error (the tag is usually local-only and unpullable).
pub fn ensure_local_image(version: &str) -> Result<()> {
    let reference = format!("ghcr.io/{}:{version}", release::REPO);
    if compose::image_present(&reference) {
        return Ok(());
    }
    bail!(
        "--no-pull was given, but {reference} is not in the local docker daemon. \
         Build and tag it first:\n  docker compose build processor\n  \
         docker tag pecan:dev {reference}\n(or drop --no-pull to pull a published tag)"
    )
}

pub fn pull_image(stack: &Stack) -> Result<()> {
    stack.compose(&["pull", "--quiet"]).map_err(|_| {
        anyhow::anyhow!(
            "image pull failed. If this is a brand-new setup, check that the GHCR package \
             ghcr.io/{} exists and is public.",
            release::REPO
        )
    })
}

pub fn install_cli_symlink(install_dir: &Path) {
    let result = (|| -> Result<()> {
        let link = compose::cli_link()?;
        std::fs::create_dir_all(link.parent().context("CLI link has no parent")?)?;
        let target = std::fs::canonicalize(install_dir)?.join("mintctl");
        if let Ok(existing) = std::fs::read_link(&link) {
            if existing == target {
                return Ok(());
            }
        }
        // symlink fails if ANY file already exists: a second install never
        // silently hijacks another installation's management command.
        std::os::unix::fs::symlink(&target, &link).with_context(|| {
            format!(
                "cannot create {} (an existing command is kept)",
                link.display()
            )
        })?;
        ui::note(format!(
            "installed the management CLI as {}",
            link.display()
        ));
        let on_path = std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|dir| Some(dir.as_path()) == link.parent())
        });
        if !on_path {
            ui::note("Add $HOME/.local/bin to PATH: export PATH=\"$HOME/.local/bin:$PATH\"");
        }
        Ok(())
    })();
    if let Err(e) = result {
        ui::warn(format!(
            "{e:#}; use {}/mintctl directly",
            install_dir.display()
        ));
    }
}

/// Poll the console through the operator's own proxy / Caddy over verified
/// HTTPS until certificates are issued and routing works.
pub fn wait_for_tls(console_domain: &str, budget: Duration) -> bool {
    wait_for_tls_url(&format!("https://{console_domain}/healthz"), budget)
}

/// Same wait for an arbitrary URL (the bundled mint's /v1/info).
pub fn wait_for_tls_url(url: &str, budget: Duration) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    let deadline = std::time::Instant::now() + budget;
    while std::time::Instant::now() < deadline {
        if agent.get(url).call().is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    false
}

fn write_env(plan: &InstallPlan) -> Result<()> {
    let now = time::OffsetDateTime::now_utc()
        .replace_millisecond(0)
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(&Rfc3339)
        .unwrap_or_default();
    let mut body = format!(
        "# Generated by the Pecan installer {now}.\n\
         # Every key is documented in .env.example.\n\
         VERSION={version}\n\
         COMPOSE_PROJECT_NAME={project}\n\
         UI_PORT={ui_port}\n\
         BIND_ADDR={bind}\n\
         COMPOSE_PROFILES={compose_profiles}\n\
         CONSOLE_DOMAIN={console_domain}\n\
         GRPC_BIND_ADDR={grpc_bind}\n\
         GRPC_PORT={grpc_port}\n\
         # First boot only; inert once users.json exists. Kept as a recovery path in\n\
         # case the printed password is lost before the operator changes it.\n\
         INITIAL_ADMIN_PASSWORD={admin_password}\n\
         ACME_EMAIL={acme_email}\n",
        version = plan.version,
        project = compose::install_project_name(&plan.install_dir),
        ui_port = plan.ui_port,
        bind = plan.bind_addr,
        compose_profiles = plan.compose_profiles(),
        console_domain = plan.console_domain,
        grpc_bind = plan.grpc_bind_addr,
        grpc_port = plan.grpc_port,
        admin_password = plan.admin_password,
        acme_email = plan.acme_email,
    );
    if let Some(attach) = &plan.attach {
        body.push_str(&format!(
            "# First boot only; inert once setup.json exists. Later changes happen in\n\
             # the console's Mint tab.\n\
             INITIAL_UNIT={unit}\n\
             INITIAL_MINT_URL={mint_url}\n\
             INITIAL_ADVERTISED_GRPC={advertised}\n",
            unit = attach.unit,
            mint_url = attach.mint_url,
            advertised = attach.advertised_grpc,
        ));
    }
    if let Some(mint_plan) = &plan.mint {
        body.push_str(&format!(
            "# The bundled mint. MINT_VERSION moves independently of VERSION:\n\
             # 'mintctl update' never touches it (the mint holds money);\n\
             # upgrade deliberately with 'mintctl update --mint-version <tag>'.\n\
             MINT_VERSION={mintd_version}\n\
             MINT_PORT={mint_port}\n\
             MINT_BIND_ADDR={mint_bind}\n\
             MINT_DOMAIN={mint_domain}\n",
            mintd_version = mint_plan.mintd_version,
            mint_port = mint_plan.mint_port,
            mint_bind = mint_plan.mint_bind_addr,
            mint_domain = mint_plan.mint_domain,
        ));
    }
    let env_path = plan.install_dir.join(".env");
    crate::storage::atomic_write(&env_path, body.as_bytes(), 0o600)?;
    Ok(())
}

pub fn next_steps(plan: &InstallPlan) -> Vec<String> {
    match (&plan.mint, &plan.attach) {
        // Bundled mint: everything is already connected.
        (Some(mint_plan), _) => vec![
            "1. Write down the mint seed shown above — it is the only recovery path.".into(),
            "2. Mint tab   — the checklist and automatic self-test confirm and lock\n     \
             the unit (give it a minute or two)."
                .into(),
            "3. Access tab — add teller accounts as needed.".into(),
            format!("4. Wallets connect to {}.", mint_plan.mint_url),
        ],
        // Pre-attached existing mint: the console form is already filled in.
        (None, Some(attach)) => vec![
            "1. Mint tab   — copy the config snippet (the attachment is already set).".into(),
            format!(
                "2. Your mintd — apply the snippet (cdk-mintd config apply) and restart it.\n     \
                 (It must run the compatible cdk release — the Mint tab shows\n     \
                 which one and why; its gRPC target is {}.)",
                attach.advertised_grpc
            ),
            "3. Mint tab   — the checklist and self-test confirm the link end to end.".into(),
            "4. Access tab — add teller accounts as needed.".into(),
        ],
        // Attach later, in the console.
        (None, None) => vec![
            "1. Mint tab   — set your unit and your mint's URL; copy the config snippet.".into(),
            format!(
                "2. Your mintd — apply the snippet (cdk-mintd config apply) and restart it.\n     \
                 (It must run the compatible cdk release — the Mint tab shows\n     \
                 which one and why; its gRPC target is {}.)",
                plan.grpc_endpoint()
            ),
            "3. Mint tab   — the checklist and self-test confirm the link end to end.".into(),
            "4. Access tab — add teller accounts as needed.".into(),
        ],
    }
}

fn print_summary(plan: &InstallPlan) {
    use ui::say;
    say("");
    say("============================================================");
    say(format!(
        " Pecan branch processor {}{} is running",
        plan.version,
        if plan.mint.is_some() { " + mint" } else { "" },
    ));
    say("============================================================");
    say("");
    say(format!("  Operator console:  {}", plan.console_url()));
    match &plan.mint {
        Some(mint_plan) => {
            let unit = plan
                .attach
                .as_ref()
                .map(|a| a.unit.as_str())
                .unwrap_or_default();
            say(format!(
                "  Mint:              {}  (wallets connect here; unit \"{unit}\")",
                mint_plan.mint_url
            ));
            say("  Payment link:      internal (processor:50051 on the compose network)");
        }
        None => {
            say(format!(
                "  Payment gRPC:      {}  (your cdk-mintd connects here)",
                plan.grpc_endpoint()
            ));
        }
    }
    say("");
    if let Some(mint_plan) = &plan.mint {
        say("  MINT SEED — write these 12 words down and store them offline.");
        say("  Anyone holding them can issue your ecash; without them the");
        say("  mint cannot be recovered.");
        say("");
        say(format!("      {}", mint_plan.mnemonic));
        say("");
        say(format!(
            "  (also stored in {}/mint/mnemonic; included in mintctl backup)",
            plan.install_dir.display()
        ));
        say("");
    }
    say(format!(
        "  Sign in:           admin / {}",
        plan.admin_password
    ));
    say(format!(
        "                     (also stored in {}/.env)",
        plan.install_dir.display()
    ));
    say("                     You will be asked to choose your own password");
    say("                     at first sign-in.");
    say("");
    say("  First steps:");
    for step in next_steps(plan) {
        say(format!("    {step}"));
    }
    say("");
    match plan.access {
        AccessMode::DomainTls => {
            say("  Certificates are provisioned automatically; the first HTTPS");
            say("  request can take a minute. Firewall: allow 80 and 443.");
        }
        AccessMode::PlainHttp => {
            say("  Plain-HTTP mode: fine on a trusted LAN, not for the public");
            match &plan.mint {
                Some(mint_plan) => say(format!(
                    "  internet. Firewall: allow {} and {}.",
                    plan.ui_port, mint_plan.mint_port
                )),
                None => say(format!("  internet. Firewall: allow {}.", plan.ui_port)),
            }
        }
        AccessMode::BehindProxy => {
            say("  Your reverse proxy terminates TLS. Ready-made server blocks:");
            say(format!(
                "    {}/proxy-snippets/  (Caddy and nginx)",
                plan.install_dir.display()
            ));
            say(format!(
                "  Proxy {} to 127.0.0.1:{}.",
                plan.console_domain, plan.ui_port
            ));
            if let Some(mint_plan) = &plan.mint {
                say(format!(
                    "  Proxy {} to 127.0.0.1:{}.",
                    mint_plan.mint_domain, mint_plan.mint_port
                ));
            }
        }
    }
    if plan.grpc_bind_addr == "0.0.0.0" {
        say("");
        say(format!(
            "  The payment gRPC ({}) is published on all interfaces and",
            plan.grpc_endpoint()
        ));
        say("  carries no authentication — firewall it to the mint's host,");
        say("  or enable TLS (GRPC_TLS_DIR in .env.example).");
    }
    say("");
    say("  Manage with: mintctl status | logs | update | backup | restore |");
    say("               start | stop | uninstall");
    say("============================================================");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_paths_support_root_and_unprivileged_users() {
        assert_eq!(
            default_install_dir(true, None).unwrap(),
            PathBuf::from("/opt/pecan")
        );
        assert_eq!(
            default_install_dir(false, Some(std::ffi::OsStr::new("/home/operator"))).unwrap(),
            PathBuf::from("/home/operator/pecan")
        );
        assert!(default_install_dir(false, None).is_err());
        assert!(default_install_dir(false, Some(std::ffi::OsStr::new("relative"))).is_err());
    }
    #[test]
    fn installs_with_the_same_basename_do_not_share_volumes() {
        assert_ne!(
            compose::install_project_name(Path::new("/home/alice/pecan")),
            compose::install_project_name(Path::new("/home/bob/pecan"))
        );
    }
    #[test]
    fn fresh_install_refuses_existing_credentials_or_mint_material() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("mint")).unwrap();
        assert!(guard_fresh_dir(dir.path()).is_err());
        std::fs::remove_dir(dir.path().join("mint")).unwrap();
        std::fs::write(dir.path().join(".env"), "VERSION=v1").unwrap();
        let error = guard_fresh_dir(dir.path()).unwrap_err().to_string();
        assert!(error.contains("start"));
    }
}
