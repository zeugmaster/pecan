//! The guided installer (and `mintctl domain`) — a clack-style wizard.
//!
//! Runs whenever install has a terminal and no `--yes`; every question has a
//! flag twin so automation never lands here. Cancelling any prompt (ESC or
//! Ctrl-C) aborts cleanly without touching the system.
//!
//! The first substantive question picks the shape: processor + a new bundled
//! mint (unit and mint hostname collected here, everything connected at the
//! end), or processor only (attach a mint you already run afterwards, in the
//! console's Mint tab).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use cliclack::{confirm, input, intro, log, note, outro, outro_cancel, select, spinner};

use crate::compose::{self, Stack};
use crate::dns;
use crate::envfile::EnvFile;
use crate::install::{self, AccessMode, AttachPlan, InstallPlan, MintPlan};
use crate::mint;
use crate::preflight::{self, PortStatus};
use crate::release;
use crate::{caddy, passphrase};
use crate::{DomainArgs, InstallArgs};

fn cancelled(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::Interrupted
}

/// Wrap a cliclack interaction: cancel becomes a clean abort.
macro_rules! step {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(ref e) if cancelled(e) => {
                let _ = outro_cancel("Cancelled — nothing was changed.");
                bail!("install cancelled");
            }
            Err(e) => return Err(e.into()),
        }
    };
}

pub fn run(args: &InstallArgs) -> Result<()> {
    intro(
        console::style(" Pecan — branch processor ")
            .on_cyan()
            .black(),
    )?;

    // --- resolve version early: everything else is pinned to it -----------
    install::preflight_platform()?;
    let sp = spinner();
    let version = match &args.version {
        Some(v) => v.clone(),
        None => {
            sp.start("Resolving the latest release ...");
            match release::resolve_latest_version() {
                Ok(v) => {
                    sp.stop(format!("Latest release: {v}"));
                    v
                }
                Err(e) => {
                    sp.error("Could not resolve the latest release");
                    return Err(e);
                }
            }
        }
    };

    // --- preflight ---------------------------------------------------------
    ensure_docker_interactive()?;

    let mut install_dir = install::resolve_install_dir(args.dir.clone())?;
    while install_dir.join(".env").is_file() {
        log::warning(format!(
            "An installation already exists in {}",
            install_dir.display()
        ))?;
        #[derive(Clone, PartialEq, Eq)]
        enum Existing {
            OtherDir,
            Abort,
        }
        match step!(select("How do you want to proceed?")
            .item(
                Existing::Abort,
                "Keep it",
                "manage it with mintctl status / update"
            )
            .item(
                Existing::OtherDir,
                "Install a second instance elsewhere",
                "choose another directory"
            )
            .interact())
        {
            Existing::Abort => {
                outro("Nothing was changed.")?;
                return Ok(());
            }
            Existing::OtherDir => {
                let dir: String = step!(input("Install directory")
                    .placeholder("./pecan-2")
                    .validate(|value: &String| {
                        if value.trim().is_empty() {
                            Err("enter a directory path")
                        } else {
                            Ok(())
                        }
                    })
                    .interact());
                install_dir = install::resolve_install_dir(Some(PathBuf::from(dir.trim())))?;
            }
        }
    }

    let sp = spinner();
    sp.start("Detecting the server's public address ...");
    let public_ip = compose::detect_public_ip();
    let public_ipv6 = compose::detect_public_ipv6();
    match (&public_ip, &public_ipv6) {
        (Some(v4), Some(v6)) => sp.stop(format!("Public IP: {v4}  (IPv6: {v6})")),
        (Some(v4), None) => sp.stop(format!("Public IP: {v4}")),
        (None, Some(v6)) => sp.stop(format!("Public IP: {v6} (IPv6 only)")),
        (None, None) => sp.stop("Public IP: could not detect (offline or LAN-only) — continuing"),
    }

    // --- what to install ----------------------------------------------------
    let with_mint = if args.with_mint {
        true
    } else {
        #[derive(Clone, PartialEq, Eq)]
        enum Shape {
            Full,
            ProcessorOnly,
        }
        let shape = step!(select("What should this server run?")
            .item(
                Shape::Full,
                "The processor and a new mint",
                "bundled cdk-mintd; everything is connected when the install finishes"
            )
            .item(
                Shape::ProcessorOnly,
                "The processor only",
                "attach a mint you already run, in the console's Mint tab"
            )
            .initial_value(Shape::Full)
            .interact());
        shape == Shape::Full
    };

    let mut ui_port = args.ui_port;
    let mut grpc_port = args.grpc_port;
    let mut mint_port = args.mint_port.unwrap_or(3338);
    let mut port_checks = vec![("console", &mut ui_port), ("payment gRPC", &mut grpc_port)];
    if with_mint {
        port_checks.push(("mint", &mut mint_port));
    }
    for (label, port) in port_checks {
        if preflight::port_status(*port) == PortStatus::Busy {
            log::warning(format!("Port {port} (for the {label}) is already in use."))?;
            let answer: String = step!(input(format!("Alternative {label} port"))
                .default_input(&port.saturating_add(10000).to_string())
                .validate(|value: &String| match value.trim().parse::<u16>() {
                    Ok(p) if p >= 1024 => Ok(()),
                    _ => Err("enter a port number (1024-65535)"),
                })
                .interact());
            *port = answer.trim().parse().unwrap_or(*port);
        }
    }

    // --- the unit (bundled mint) -------------------------------------------
    let unit = if with_mint {
        let preset = args.unit.clone().unwrap_or_default();
        let entered: String = if preset.is_empty() {
            step!(input("Currency unit this mint issues")
                .placeholder("ora")
                .validate(|value: &String| {
                    match mint::validate_unit_slug(value) {
                        Ok(_) => Ok(()),
                        Err(_) => Err("lowercase letters, digits, - and _ only (e.g. \"ora\")"),
                    }
                })
                .interact())
        } else {
            preset
        };
        let unit = mint::validate_unit_slug(&entered)?;
        if mint::RESERVED_UNITS.contains(&unit.as_str()) {
            log::warning(format!(
                "\"{unit}\" is a unit wallets treat specially — a name that is \
                 unambiguously yours avoids confusion."
            ))?;
        }
        Some(unit)
    } else {
        None
    };

    // --- where the mint runs (processor-only shape) -------------------------
    let mut grpc_bind_addr = args.grpc_bind.clone().unwrap_or_default();
    if with_mint {
        // The bundled mint dials processor:50051 over the compose network;
        // the host publish stays loopback-only.
        grpc_bind_addr = "127.0.0.1".into();
    } else if grpc_bind_addr.is_empty() {
        #[derive(Clone, PartialEq, Eq)]
        enum MintLocation {
            SameHost,
            OtherMachine,
        }
        let location = step!(select("Where does (or will) your cdk-mintd run?")
            .item(
                MintLocation::SameHost,
                "On this server",
                "the payment gRPC stays on localhost"
            )
            .item(
                MintLocation::OtherMachine,
                "On another machine in the private network",
                "publishes the gRPC on all interfaces — firewall it or enable TLS"
            )
            .interact());
        grpc_bind_addr = match location {
            MintLocation::SameHost => "127.0.0.1".into(),
            MintLocation::OtherMachine => {
                log::warning(format!(
                    "The payment gRPC (port {grpc_port}) carries no authentication.\n\
                     Restrict it to your private network with a firewall rule, or\n\
                     enable mutual TLS (GRPC_TLS_DIR in .env.example)."
                ))?;
                "0.0.0.0".into()
            }
        };
    }

    // --- console (and mint) access ------------------------------------------
    let ports_busy = preflight::ports_80_443_busy();
    let access = if args.behind_proxy {
        AccessMode::BehindProxy
    } else if args.plain_http {
        AccessMode::PlainHttp
    } else if args.console_domain.is_some() && !ports_busy {
        AccessMode::DomainTls
    } else {
        if ports_busy {
            log::info(
                "Something on this server already listens on port 80/443 —\n\
                 an existing reverse proxy, most likely. The console can run\n\
                 behind it instead of bringing its own.",
            )?;
        }
        let reach_question = if with_mint {
            "How should operators (and wallets) reach this server?"
        } else {
            "How should operators reach the console?"
        };
        let tls_hint = if with_mint {
            "bundled Caddy obtains certificates for the console and mint hostnames"
        } else {
            "bundled Caddy obtains certificates for the console hostname"
        };
        let mut sel = select(reach_question)
            .item(
                AccessMode::DomainTls,
                if ports_busy {
                    "Domain with automatic HTTPS (needs ports 80/443)"
                } else {
                    "Domain with automatic HTTPS (recommended)"
                },
                tls_hint,
            )
            .item(
                AccessMode::BehindProxy,
                if ports_busy {
                    "Behind my own reverse proxy (recommended here)"
                } else {
                    "Behind my own reverse proxy"
                },
                "binds to localhost; you get ready-made Caddy/nginx snippets",
            )
            .item(AccessMode::PlainHttp, "Plain HTTP", "LAN or testing only");
        sel = sel.initial_value(if ports_busy {
            AccessMode::BehindProxy
        } else {
            AccessMode::DomainTls
        });
        step!(sel.interact())
    };

    let mut console_domain = args.console_domain.clone().unwrap_or_default();
    let mut mint_domain = args.mint_domain.clone().unwrap_or_default();
    let mut acme_email = args.email.clone().unwrap_or_default();
    let mut access = access;
    if access != AccessMode::PlainHttp {
        console_domain = collect_console_domain(&console_domain)?;
        if with_mint {
            mint_domain = collect_mint_domain(&mint_domain, &console_domain)?;
        }
    }
    match access {
        AccessMode::DomainTls => {
            if ports_busy {
                log::warning(
                    "Ports 80/443 are in use — the bundled Caddy will not be able to bind them.",
                )?;
                if !step!(confirm("Continue with the bundled Caddy anyway?")
                    .initial_value(false)
                    .interact())
                {
                    log::step("Switching to behind-your-own-proxy mode.")?;
                    access = AccessMode::BehindProxy;
                }
            }
        }
        AccessMode::PlainHttp | AccessMode::BehindProxy => {}
    }
    if access == AccessMode::DomainTls {
        if acme_email.is_empty() {
            acme_email = step!(input("Email for certificate expiry notices (optional)")
                .placeholder("you@example.org")
                .required(false)
                .validate(|value: &String| {
                    let v = value.trim();
                    if v.is_empty() || (v.contains('@') && !v.contains(char::is_whitespace)) {
                        Ok(())
                    } else {
                        Err("enter an email address or leave empty")
                    }
                })
                .interact());
            acme_email = acme_email.trim().to_string();
        }
        let mut domains = vec![console_domain.as_str()];
        if with_mint {
            domains.push(mint_domain.as_str());
        }
        confirm_dns(
            &domains,
            public_ip.as_deref(),
            public_ipv6.as_deref(),
            &mut access,
        )?;
    }

    // --- build the plan -----------------------------------------------------
    let (attach, mint_plan) = if with_mint {
        let unit = unit.expect("with_mint collects a unit");
        let mint_url = match &args.mint_url {
            Some(url) => url.trim_end_matches('/').to_string(),
            None => match access {
                AccessMode::DomainTls | AccessMode::BehindProxy => format!("https://{mint_domain}"),
                AccessMode::PlainHttp => match &public_ip {
                    Some(ip) => format!("http://{ip}:{mint_port}"),
                    None => {
                        let entered: String = step!(input(
                            "Address wallets use to reach the mint (no public IP detected)"
                        )
                        .placeholder(&format!("http://192.168.1.10:{mint_port}"))
                        .validate(|value: &String| {
                            let v = value.trim();
                            if v.starts_with("http://") || v.starts_with("https://") {
                                Ok(())
                            } else {
                                Err("enter a full URL, e.g. http://192.168.1.10:3338")
                            }
                        })
                        .interact());
                        entered.trim().trim_end_matches('/').to_string()
                    }
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
                    mint_domain.clone()
                },
                mint_url,
                mint_port,
                mint_bind_addr: match access {
                    AccessMode::PlainHttp => "0.0.0.0".into(),
                    _ => "127.0.0.1".into(),
                },
                mintd_version: args
                    .mint_version
                    .clone()
                    .unwrap_or_else(|| mint::MINTD_DEFAULT_TAG.into()),
                mnemonic: mint::generate_mnemonic(),
            }),
        )
    } else {
        (None, None)
    };

    let plan = InstallPlan {
        bind_addr: args.bind.clone().unwrap_or_else(|| {
            match access {
                AccessMode::PlainHttp => "0.0.0.0",
                _ => "127.0.0.1",
            }
            .to_string()
        }),
        install_dir,
        version,
        no_pull: args.no_pull,
        access,
        console_domain: if access == AccessMode::PlainHttp {
            String::new()
        } else {
            console_domain
        },
        acme_email,
        ui_port,
        grpc_bind_addr,
        grpc_port,
        public_ip,
        admin_password: passphrase::generate(),
        attach,
        mint: mint_plan,
    };

    // --- summary + confirm --------------------------------------------------
    let access_line = match plan.access {
        AccessMode::DomainTls => format!("{} — automatic HTTPS", plan.console_url()),
        AccessMode::BehindProxy => format!("{} — via your own reverse proxy", plan.console_url()),
        AccessMode::PlainHttp => format!("{} — plain HTTP (LAN/testing)", plan.console_url()),
    };
    let body = match (&plan.mint, &plan.attach) {
        (Some(mint_plan), Some(attach)) => format!(
            "Console        {access_line}\n\
             Mint           {mint_url} — unit \"{unit}\" (wallets connect here)\n\
             Mint image     cashubtc/mintd:{mintd_version}\n\
             Payment link   internal (processor:50051 on the compose network)\n\
             Directory      {dir}\n\
             Version        {version}\n\
             \n\
             A new mint seed (12 words) will be generated and shown once.",
            mint_url = mint_plan.mint_url,
            unit = attach.unit,
            mintd_version = mint_plan.mintd_version,
            dir = plan.install_dir.display(),
            version = plan.version,
        ),
        _ => format!(
            "Console        {access_line}\n\
             Payment gRPC   {grpc} (your cdk-mintd connects here)\n\
             Directory      {dir}\n\
             Version        {version}\n\
             \n\
             The mint itself is attached afterwards, in the console's Mint tab.",
            grpc = plan.grpc_endpoint(),
            dir = plan.install_dir.display(),
            version = plan.version,
        ),
    };
    note("Ready to install", body)?;
    if !step!(confirm("Install now?").initial_value(true).interact()) {
        outro_cancel("Cancelled — nothing was changed.")?;
        bail!("install cancelled");
    }

    execute_with_progress(&plan, args)?;
    finish(&plan)?;
    Ok(())
}

fn collect_mint_domain(preset: &str, console_domain: &str) -> Result<String> {
    if !preset.is_empty() && preset != console_domain {
        return Ok(preset.to_string());
    }
    let console = console_domain.to_string();
    let entered: String = step!(input("Hostname for the mint (wallets connect here)")
        .placeholder("mint.example.org")
        .validate(move |value: &String| {
            let v = value.trim();
            if !dns::valid_hostname(v) {
                Err("enter a bare hostname like mint.example.org (no https://, lowercase)")
            } else if v == console {
                Err("the mint needs its own hostname, different from the console's")
            } else {
                Ok(())
            }
        })
        .interact());
    Ok(entered.trim().to_string())
}

fn collect_console_domain(preset: &str) -> Result<String> {
    if !preset.is_empty() {
        return Ok(preset.to_string());
    }
    let entered: String = step!(input("Hostname for the operator console")
        .placeholder("console.example.org")
        .validate(|value: &String| {
            if dns::valid_hostname(value.trim()) {
                Ok(())
            } else {
                Err("enter a bare hostname like console.example.org (no https://, lowercase)")
            }
        })
        .interact());
    Ok(entered.trim().to_string())
}

/// Show the required record(s) as a copy-ready table, then live-poll public
/// DNS until every domain resolves to this server (or the operator decides
/// otherwise). AAAA records are advisory: optional to create, but actively
/// warned about when one exists and points away from this server, because
/// Let's Encrypt prefers IPv6 the moment an AAAA exists.
fn confirm_dns(
    domains: &[&str],
    public_ip: Option<&str>,
    public_ipv6: Option<&str>,
    access: &mut AccessMode,
) -> Result<()> {
    let ip_hint = public_ip.unwrap_or("<this server's IP>");
    let width = domains.iter().map(|d| d.len()).max().unwrap_or(0);
    let mut lines: Vec<String> = domains
        .iter()
        .map(|domain| format!("A     {domain:<width$}  →  {ip_hint}"))
        .collect();
    if let Some(v6) = public_ipv6 {
        lines.push(String::new());
        lines.extend(
            domains
                .iter()
                .map(|domain| format!("AAAA  {domain:<width$}  →  {v6}   (optional)")),
        );
        lines.push(String::new());
        lines.push(
            "The A records are required. AAAA is optional — add it only if IPv6\n\
             to this server actually works; a wrong AAAA breaks certificates."
                .into(),
        );
    }
    lines.push(String::new());
    lines.push("Ports 80 and 443 must be reachable from the internet.".into());
    note(
        format!(
            "Create with your DNS provider — record{} pointing at this server",
            if domains.len() > 1 { "s" } else { "" }
        ),
        lines.join("\n"),
    )?;

    loop {
        let sp = spinner();
        sp.start("Checking DNS via public resolvers (1.1.1.1, 8.8.8.8) ...");
        let checks: Vec<_> = domains
            .iter()
            .map(|domain| dns::check(domain, public_ip).ok())
            .collect();
        if checks.iter().all(|c| c.as_ref().is_some_and(|c| c.matches)) {
            sp.stop(format!(
                "DNS looks right — the record{} at this server.",
                if domains.len() > 1 {
                    "s point"
                } else {
                    " points"
                }
            ));
            for domain in domains {
                if let Some(advisory) = dns::aaaa_advisory(domain, public_ipv6) {
                    log::warning(advisory)?;
                }
            }
            return Ok(());
        }
        sp.stop("DNS is not confirmed yet:");
        let mut any_mismatch = false;
        for check in checks.iter().flatten() {
            let status = if check.matches {
                "ok".to_string()
            } else if check.resolved.is_empty() {
                "no record found".to_string()
            } else {
                any_mismatch = true;
                let ips: Vec<String> = check.resolved.iter().map(|ip| ip.to_string()).collect();
                format!(
                    "resolves to {} — not the detected address ({})",
                    ips.join(", "),
                    public_ip.unwrap_or("unknown")
                )
            };
            log::step(format!("{}  —  {status}", check.domain))?;
            if check.behind_cloudflare {
                log::warning(format!(
                    "{} appears to be behind the Cloudflare proxy (orange cloud).\n\
                     Certificate issuance needs the record set to \"DNS only\" (grey cloud).",
                    check.domain
                ))?;
            }
            if let Some(advisory) = dns::aaaa_advisory(&check.domain, public_ipv6) {
                log::warning(advisory)?;
            }
        }
        if any_mismatch {
            log::info(
                "If the resolved address IS this server (several IPs, NAT, or the\n\
                 detection answered over IPv6), choose \"Continue anyway\" — certificates\n\
                 only need the record to actually reach this machine.",
            )?;
        } else {
            log::info("Freshly created records can take a few minutes to propagate.")?;
        }

        #[derive(Clone, PartialEq, Eq)]
        enum Next {
            Recheck,
            Continue,
            Fallback,
            Abort,
        }
        match step!(select("How do you want to proceed?")
            .item(
                Next::Recheck,
                "Check again",
                "after creating or fixing the record"
            )
            .item(
                Next::Continue,
                "Continue anyway",
                "certificates will be retried in the background once DNS is right"
            )
            .item(
                Next::Fallback,
                "Switch to plain HTTP",
                "skip TLS for now; mintctl domain can set it up later"
            )
            .item(
                Next::Abort,
                "Abort the install",
                "nothing has been changed yet"
            )
            .interact())
        {
            Next::Recheck => continue,
            Next::Continue => return Ok(()),
            Next::Fallback => {
                *access = AccessMode::PlainHttp;
                return Ok(());
            }
            Next::Abort => {
                outro_cancel("Cancelled — nothing was changed.")?;
                bail!("install cancelled");
            }
        }
    }
}

fn ensure_docker_interactive() -> Result<()> {
    let allow_install =
        if !compose::docker_available() && compose::is_root() && cfg!(target_os = "linux") {
            log::warning("Docker is not installed.")?;
            step!(confirm("Install Docker now via https://get.docker.com?")
                .initial_value(true)
                .interact())
        } else {
            false
        };
    compose::ensure_docker(allow_install)?;
    log::success("Docker is ready.")?;
    Ok(())
}

fn execute_with_progress(plan: &InstallPlan, args: &InstallArgs) -> Result<()> {
    if plan.no_pull {
        // Before anything is written: a missing local image should not
        // leave a half-finished install behind.
        install::ensure_local_image(&plan.version)?;
    }
    install::guard_fresh_dir(&plan.install_dir)?;
    let _lock = crate::storage::lock(&plan.install_dir)?;
    let source = install::artifact_source(
        args.artifacts_dir.clone(),
        args.artifact_ref.clone(),
        &plan.version,
    );

    let sp = spinner();
    sp.start(format!(
        "Fetching the {} deployment artifacts ...",
        plan.version
    ));
    install::prepare_install(&source, plan)?;
    sp.stop(format!(
        "Deployment artifacts in {}",
        plan.install_dir.display()
    ));

    let stack = Stack {
        install_dir: plan.install_dir.clone(),
    };
    let images = if plan.mint.is_some() {
        "images"
    } else {
        "image"
    };
    if plan.no_pull {
        log::step(format!(
            "Skipping the image pull (--no-pull); using the local {} image.",
            plan.version
        ))?;
    } else {
        let sp = spinner();
        sp.start(format!(
            "Pulling the container {images} ({}) ...",
            plan.version
        ));
        match stack.compose_quiet(&["pull", "--quiet"]) {
            Ok(()) => sp.stop(format!(
                "{} pulled.",
                if plan.mint.is_some() {
                    "Images"
                } else {
                    "Image"
                }
            )),
            Err(e) => {
                sp.error("Image pull failed");
                return Err(e);
            }
        }
    }

    if plan.mint.is_some() {
        let sp = spinner();
        sp.start("Validating the mint configuration ...");
        match install::validate_mint_config(&stack) {
            Ok(()) => sp.stop("Mint configuration is valid."),
            Err(e) => {
                sp.error("The mint configuration did not validate");
                return Err(e);
            }
        }
    }

    let sp = spinner();
    sp.start("Starting the stack ...");
    if let Err(e) = stack.compose_quiet(&[
        "up",
        "-d",
        "--pull",
        "never",
        "--wait",
        "--wait-timeout",
        "120",
    ]) {
        sp.error("The stack did not start");
        return Err(e);
    }
    sp.stop("Stack started.");

    if plan.access == AccessMode::DomainTls {
        let sp = spinner();
        sp.start(format!(
            "Waiting for Let's Encrypt certificates for {} (up to 3 minutes) ...",
            plan.console_domain
        ));
        if install::wait_for_tls(&plan.console_domain, Duration::from_secs(180)) {
            sp.stop(format!("HTTPS is live at {}", plan.console_url()));
        } else {
            sp.stop("Certificates are still pending — Caddy keeps retrying in the background.");
            log::info(format!(
                "Once DNS is right, https://{} will come up by itself.\n\
                 Watch progress with: {}/mintctl logs caddy",
                plan.console_domain,
                plan.install_dir.display()
            ))?;
        }
        if let Some(mint_plan) = &plan.mint {
            let sp = spinner();
            sp.start(format!(
                "Waiting for certificates for {} ...",
                mint_plan.mint_domain
            ));
            let url = format!("https://{}/v1/info", mint_plan.mint_domain);
            if install::wait_for_tls_url(&url, Duration::from_secs(180)) {
                sp.stop(format!("HTTPS is live at {}", mint_plan.mint_url));
            } else {
                sp.stop("Mint certificates are still pending — Caddy keeps retrying.");
            }
        }
    }
    Ok(())
}

fn finish(plan: &InstallPlan) -> Result<()> {
    let mut body = match (&plan.mint, &plan.attach) {
        (Some(mint_plan), Some(attach)) => format!(
            "Operator console   {console}\n\
             Mint               {mint_url}  (wallets connect here; unit \"{unit}\")\n\n\
             Sign in as         admin\n\
             Password           {password}\n\
             (also stored in {dir}/.env — you will choose\n\
             your own password at first sign-in)\n\n\
             MINT SEED — write these 12 words down and store them offline.\n\
             Anyone holding them can issue your ecash; without them the\n\
             mint cannot be recovered.\n\n\
             \x20   {mnemonic}\n\n\
             (also stored in {dir}/mint/mnemonic; included in mintctl backup)\n\nNext steps:",
            console = plan.console_url(),
            mint_url = mint_plan.mint_url,
            unit = attach.unit,
            password = plan.admin_password,
            dir = plan.install_dir.display(),
            mnemonic = mint_plan.mnemonic,
        ),
        _ => format!(
            "Operator console   {console}\n\
             Payment gRPC       {grpc}  (your cdk-mintd connects here)\n\n\
             Sign in as         admin\n\
             Password           {password}\n\
             (also stored in {dir}/.env — you will choose\n\
             your own password at first sign-in)\n\nNext steps:",
            console = plan.console_url(),
            grpc = plan.grpc_endpoint(),
            password = plan.admin_password,
            dir = plan.install_dir.display(),
        ),
    };
    for step in install::next_steps(plan) {
        body.push_str(&format!("\n  {step}"));
    }
    if plan.access == AccessMode::BehindProxy {
        body.push_str(&format!(
            "\n\nYour reverse proxy terminates TLS — ready-made server blocks\n\
             (console{and_mint}):\n  \
             {dir}/proxy-snippets/  (Caddy and nginx)",
            and_mint = if plan.mint.is_some() { " and mint" } else { "" },
            dir = plan.install_dir.display()
        ));
    }
    note(
        format!(
            "Branch processor {}{} is running",
            plan.version,
            if plan.mint.is_some() { " + mint" } else { "" }
        ),
        body,
    )?;
    outro("Manage with: mintctl status | logs | update | domain | backup | restore | start | stop | uninstall")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// mintctl domain — re-run the console-access step on an existing install
// ---------------------------------------------------------------------------

pub fn domain_command(args: &DomainArgs) -> Result<()> {
    let stack = Stack::discover()?;
    let _lock = crate::storage::lock(&stack.install_dir)?;
    let mut envf = EnvFile::load(&stack.env_path())?;
    let interactive = !args.yes && crate::ui::have_tty();

    let current_domain = envf.get("CONSOLE_DOMAIN").unwrap_or_default();
    let ui_port: u16 = envf
        .get("UI_PORT")
        .and_then(|p| p.parse().ok())
        .unwrap_or(9090);
    // A bundled-mint install keeps its mint profile and gets the mint's
    // hostname carried through every access change.
    let has_mint = envf
        .get("COMPOSE_PROFILES")
        .unwrap_or_default()
        .split(',')
        .any(|p| p.trim() == "mint");
    let current_mint_domain = envf.get("MINT_DOMAIN").unwrap_or_default();
    let mint_port: u16 = envf
        .get("MINT_PORT")
        .and_then(|p| p.parse().ok())
        .unwrap_or(3338);

    let (access, console_domain, mint_domain, acme_email) = if interactive {
        intro(console::style(" Console domain & TLS ").on_cyan().black())?;
        if current_domain.is_empty() {
            log::info("Currently: plain HTTP (no domain).")?;
        } else {
            log::info(format!("Currently: https://{current_domain}"))?;
        }
        let ports_busy = preflight::ports_80_443_busy();
        let access = step!(select("How should operators reach the console?")
            .item(
                AccessMode::DomainTls,
                "Domain with automatic HTTPS",
                "bundled Caddy obtains certificates"
            )
            .item(
                AccessMode::BehindProxy,
                "Behind my own reverse proxy",
                "binds to localhost; you get ready-made snippets"
            )
            .item(AccessMode::PlainHttp, "Plain HTTP", "LAN or testing only")
            .initial_value(if !current_domain.is_empty() {
                AccessMode::DomainTls
            } else if ports_busy {
                AccessMode::BehindProxy
            } else {
                AccessMode::DomainTls
            })
            .interact());
        let mut access = access;
        let mut console_domain = String::new();
        let mut mint_domain = String::new();
        // Preserved across access-mode switches; only DomainTls edits it.
        let mut email = envf.get("ACME_EMAIL").unwrap_or_default();
        if access != AccessMode::PlainHttp {
            let preset = args.console_domain.clone().unwrap_or_default();
            console_domain = collect_console_domain(&preset)?;
            if has_mint {
                let preset = args
                    .mint_domain
                    .clone()
                    .unwrap_or_else(|| current_mint_domain.clone());
                mint_domain = collect_mint_domain(&preset, &console_domain)?;
            }
        }
        if access == AccessMode::DomainTls {
            if let Some(flag_email) = &args.email {
                email = flag_email.clone();
            }
            let public_ip = compose::detect_public_ip();
            let public_ipv6 = compose::detect_public_ipv6();
            let mut domains = vec![console_domain.as_str()];
            if has_mint {
                domains.push(mint_domain.as_str());
            }
            confirm_dns(
                &domains,
                public_ip.as_deref(),
                public_ipv6.as_deref(),
                &mut access,
            )?;
        }
        (access, console_domain, mint_domain, email)
    } else {
        let console_domain = args.console_domain.clone().unwrap_or_default();
        let access = if args.behind_proxy {
            if console_domain.is_empty() {
                bail!("--behind-proxy needs --console-domain");
            }
            AccessMode::BehindProxy
        } else if args.plain_http || console_domain.is_empty() {
            if !args.plain_http && console_domain.is_empty() {
                bail!("pass --console-domain <d>, --plain-http, or --behind-proxy (with --console-domain)");
            }
            AccessMode::PlainHttp
        } else {
            AccessMode::DomainTls
        };
        if access != AccessMode::PlainHttp && !dns::valid_hostname(&console_domain) {
            bail!("--console-domain {console_domain} is not a valid hostname");
        }
        let mint_domain = if access == AccessMode::PlainHttp {
            String::new()
        } else if has_mint {
            let domain = args
                .mint_domain
                .clone()
                .unwrap_or_else(|| current_mint_domain.clone());
            if domain.is_empty() {
                bail!("this install bundles a mint — pass --mint-domain <hostname> as well");
            }
            if !dns::valid_hostname(&domain) {
                bail!("--mint-domain {domain} is not a valid hostname");
            }
            domain
        } else {
            String::new()
        };
        let email = args
            .email
            .clone()
            .unwrap_or_else(|| envf.get("ACME_EMAIL").unwrap_or_default());
        (access, console_domain, mint_domain, email)
    };

    let (tls_profile, bind) = match access {
        AccessMode::DomainTls => (Some("tls"), "127.0.0.1"),
        AccessMode::BehindProxy => (None, "127.0.0.1"),
        AccessMode::PlainHttp => (None, "0.0.0.0"),
    };
    let mut profiles: Vec<&str> = tls_profile.into_iter().collect();
    if has_mint {
        profiles.push("mint");
    }
    envf.set("COMPOSE_PROFILES", &profiles.join(","));
    envf.set("BIND_ADDR", bind);
    envf.set("CONSOLE_DOMAIN", &console_domain);
    envf.set("ACME_EMAIL", &acme_email);
    if has_mint {
        envf.set("MINT_DOMAIN", &mint_domain);
        envf.set(
            "MINT_BIND_ADDR",
            if access == AccessMode::PlainHttp {
                "0.0.0.0"
            } else {
                "127.0.0.1"
            },
        );
    }
    envf.save()?;
    caddy::apply_acme_email(&stack.install_dir, &acme_email)?;
    caddy::apply_mint_site(
        &stack.install_dir,
        has_mint && access == AccessMode::DomainTls && !mint_domain.is_empty(),
    )?;
    if access == AccessMode::BehindProxy {
        let snippet_plan = snippet_plan(
            &stack,
            &console_domain,
            ui_port,
            has_mint.then(|| (mint_domain.clone(), mint_port)),
        );
        caddy::write_proxy_snippets(&snippet_plan)?;
    }
    if has_mint {
        crate::ui::say(
            "Heads-up: if the mint's hostname changed, its public URL changed too — \
             update the mint URL in the console's Mint tab (and re-issue wallets the \
             new address).",
        );
    }

    let apply = |quiet: bool| -> Result<()> {
        if quiet {
            stack.compose_quiet(&["up", "-d", "--remove-orphans"])
        } else {
            stack.compose(&["up", "-d", "--remove-orphans"])
        }
    };
    if interactive {
        let sp = spinner();
        sp.start("Applying the new access configuration ...");
        if let Err(e) = apply(true) {
            sp.error("Could not apply the configuration");
            return Err(e);
        }
        if !compose::wait_healthy(ui_port, Duration::from_secs(120)) {
            sp.error("The console did not come back");
            bail!("check 'mintctl logs processor'");
        }
        sp.stop("Configuration applied.");
        if access == AccessMode::DomainTls {
            let sp = spinner();
            sp.start(format!(
                "Waiting for certificates for {console_domain} (up to 3 minutes) ..."
            ));
            if install::wait_for_tls(&console_domain, Duration::from_secs(180)) {
                sp.stop(format!("HTTPS is live at https://{console_domain}"));
            } else {
                sp.stop("Certificates are still pending — Caddy keeps retrying in the background.");
            }
        }
        outro("Done.")?;
    } else {
        apply(false)?;
        if !compose::wait_healthy(ui_port, Duration::from_secs(120)) {
            bail!("the console did not come back — check 'mintctl logs processor'");
        }
        crate::ui::say("Access configuration applied.");
    }
    Ok(())
}

/// A minimal InstallPlan for snippet rendering on an existing install.
fn snippet_plan(
    stack: &Stack,
    console_domain: &str,
    ui_port: u16,
    mint: Option<(String, u16)>,
) -> InstallPlan {
    InstallPlan {
        install_dir: stack.install_dir.clone(),
        version: String::new(),
        no_pull: true,
        access: AccessMode::BehindProxy,
        console_domain: console_domain.to_string(),
        acme_email: String::new(),
        ui_port,
        bind_addr: "127.0.0.1".into(),
        grpc_bind_addr: "127.0.0.1".into(),
        grpc_port: 50051,
        public_ip: None,
        admin_password: String::new(),
        attach: None,
        mint: mint.map(|(mint_domain, mint_port)| MintPlan {
            mint_url: format!("https://{mint_domain}"),
            mint_domain,
            mint_port,
            mint_bind_addr: "127.0.0.1".into(),
            mintd_version: String::new(),
            mnemonic: String::new(),
        }),
    }
}
