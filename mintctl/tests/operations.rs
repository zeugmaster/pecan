//! Exercise the actual CLI against a controlled Docker executable. Failures
//! must not switch live pins before the complete deployment is recoverable.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _temp: tempfile::TempDir,
    install: PathBuf,
    artifacts: PathBuf,
    bin: PathBuf,
    log: PathBuf,
}
impl Fixture {
    fn new(bundled: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("installed pecan");
        let artifacts = temp.path().join("release");
        let bin = temp.path().join("bin");
        for dir in [&install, &artifacts, &bin] {
            fs::create_dir(dir).unwrap();
        }
        for name in ["docker-compose.yml", "Caddyfile", ".env.example"] {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join(name);
            fs::copy(&source, install.join(name)).unwrap();
            fs::copy(&source, artifacts.join(name)).unwrap();
        }
        fs::write(
            artifacts.join(".env.example"),
            "#MINT_VERSION=mint-tested\n",
        )
        .unwrap();
        fs::write(install.join(".env"), format!("VERSION=v1\nCOMPOSE_PROJECT_NAME=original-project\n# keep operator settings\nCUSTOM=value\nCOMPOSE_PROFILES={}\n{}",
            if bundled { "mint" } else { "" }, if bundled { "MINT_VERSION=mint-old\n" } else { "" })).unwrap();
        fs::set_permissions(install.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_mintctl"), install.join("mintctl")).unwrap();
        let docker = bin.join("docker");
        fs::write(
            &docker,
            r##"#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ['TEST_LOG'], 'a') as log:
    log.write(json.dumps({'args': args, 'version_env': os.getenv('VERSION')}) + '\n')
if args[:2] == ['compose', 'up'] and '--help' in args:
    print('--wait --wait-timeout --pull'); sys.exit(0)
if args[0] in ['info', '--version', 'image'] or args[:2] == ['compose', 'version']:
    sys.exit(0)
if args[0] == 'compose':
    action = next(a for a in args if a in ['config', 'pull', 'up', 'ps', 'stop', 'start', 'down'])
    if action == 'config' and '--images' in args:
        print('fixture-image')
    if action == 'ps':
        print('processor\nmintd')
    if action == os.getenv('TEST_FAIL'):
        sys.exit(23)
if args[0] == 'run':
    if os.getenv('TEST_FAIL') == 'backup': sys.exit(24)
    sys.stdout.buffer.write(b'fixture snapshot')
sys.exit(0)
"##,
        )
        .unwrap();
        fs::set_permissions(docker, fs::Permissions::from_mode(0o755)).unwrap();
        let log = temp.path().join("docker.log");
        Self {
            _temp: temp,
            install,
            artifacts,
            bin,
            log,
        }
    }
    fn run(&self, args: &[&str], failure: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mintctl"))
            .args(args)
            .env("MINTCTL_DIR", &self.install)
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("TEST_LOG", &self.log)
            .env("TEST_FAIL", failure)
            .env("VERSION", "unrelated-shell-version")
            .current_dir(self._temp.path())
            .output()
            .unwrap()
    }
    fn update(&self, extra: &[&str], failure: &str) -> Output {
        let mut args = vec![
            "update",
            "--yes",
            "--artifacts-dir",
            self.artifacts.to_str().unwrap(),
        ];
        args.extend(extra);
        self.run(&args, failure)
    }
    fn env(&self) -> String {
        fs::read_to_string(self.install.join(".env")).unwrap()
    }
    fn commands(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn recovery(&self) -> PathBuf {
        fs::read_dir(self.install.join("updates"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn preview_is_read_only_and_includes_the_target_releases_tested_mint() {
    let f = Fixture::new(true);
    let before = f.env();
    let output = f.update(&["--version", "v2", "--with-mint", "--check"], "");
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("mint-tested"));
    assert_eq!(before, f.env());
    assert!(!f.install.join(".mintctl.lock").exists());
    assert!(f.commands().is_empty());
}

#[test]
fn failed_pull_preserves_all_live_files_and_never_stops_services() {
    let f = Fixture::new(true);
    let before = f.env();
    let compose = fs::read(f.install.join("docker-compose.yml")).unwrap();
    let output = f.update(&["--version", "v2", "--mint-version", "mint-new"], "pull");
    assert!(!output.status.success());
    assert_eq!(before, f.env());
    assert_eq!(
        compose,
        fs::read(f.install.join("docker-compose.yml")).unwrap()
    );
    assert!(!f.install.join("updates").exists());
    assert!(!f.commands().iter().any(|c| c["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "stop" || a == "--force-recreate")));
}

#[test]
fn missing_artifact_preserves_the_installation() {
    let f = Fixture::new(false);
    let before = f.env();
    fs::remove_file(f.artifacts.join("Caddyfile")).unwrap();
    assert!(!f.update(&["--version", "v2"], "").status.success());
    assert_eq!(before, f.env());
    assert!(f.commands().is_empty());
}

#[test]
fn mint_only_update_does_not_resolve_or_change_console_release() {
    let f = Fixture::new(true);
    let compose = fs::read(f.install.join("docker-compose.yml")).unwrap();
    fs::remove_dir_all(&f.artifacts).unwrap(); // This path needs no release artifacts/network.
    success(&f.update(&["--mint-version", "mint-new"], ""));
    assert!(f.env().contains("VERSION=v1\n"));
    assert!(f.env().contains("MINT_VERSION=mint-new\n"));
    assert_eq!(
        compose,
        fs::read(f.install.join("docker-compose.yml")).unwrap()
    );
    let commands = f.commands();
    let up = commands
        .iter()
        .find(|c| {
            c["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "--force-recreate")
        })
        .unwrap();
    assert_eq!(up["args"].as_array().unwrap().last().unwrap(), "mintd");
    assert!(!up["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "processor"));
}

#[test]
fn console_update_keeps_mint_pin_and_preserves_recovery_files() {
    let f = Fixture::new(true);
    let before = f.env();
    success(&f.update(&["--version", "v2"], ""));
    assert!(f.env().contains("VERSION=v2\n"));
    assert!(f.env().contains("MINT_VERSION=mint-old\n"));
    assert!(f.env().contains("# keep operator settings\nCUSTOM=value\n"));
    let recovery = f.recovery();
    assert_eq!(fs::read_to_string(recovery.join(".env")).unwrap(), before);
    assert_eq!(
        fs::metadata(recovery.join("backup.tar.gz"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&recovery).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let commands = f.commands();
    for command in commands.iter().filter(|c| {
        c["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "--project-name")
    }) {
        assert_eq!(command["version_env"], serde_json::Value::Null);
        assert!(command["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "original-project"));
    }
    let up = commands
        .iter()
        .find(|c| {
            c["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "--force-recreate")
        })
        .unwrap();
    assert_eq!(up["args"].as_array().unwrap().last().unwrap(), "processor");
    assert!(!up["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "mintd" || a == "--remove-orphans"));
}

#[test]
fn backup_failure_restarts_services_without_switching_versions() {
    let f = Fixture::new(true);
    let before = f.env();
    assert!(!f.update(&["--version", "v2"], "backup").status.success());
    assert_eq!(before, f.env());
    let commands = f.commands();
    assert!(commands.last().unwrap()["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "start"));
    assert!(!f.recovery().join("backup.tar.gz").exists());
}

#[test]
fn startup_failure_keeps_target_pins_and_snapshot_for_explicit_recovery() {
    let f = Fixture::new(true);
    let before = f.env();
    let output = f.update(&["--version", "v2", "--with-mint"], "up");
    assert!(!output.status.success());
    assert!(f.env().contains("VERSION=v2\n"));
    assert!(f.env().contains("MINT_VERSION=mint-tested\n"));
    assert_eq!(
        fs::read_to_string(f.recovery().join(".env")).unwrap(),
        before
    );
    assert!(f.recovery().join("backup.tar.gz").exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No database downgrade"));
}

#[test]
fn refuses_legacy_managed_mints_and_external_mint_updates() {
    let f = Fixture::new(false);
    assert!(!f
        .update(&["--mint-version", "mint-new"], "")
        .status
        .success());
    fs::write(f.install.join(".env"), "VERSION=v1\nMINT_MODE=managed\n").unwrap();
    assert!(!f.update(&["--version", "v2"], "").status.success());
    assert!(f.commands().is_empty());
}

#[test]
fn concurrent_mutation_is_refused() {
    let f = Fixture::new(false);
    let lock = fs::File::create(f.install.join(".mintctl.lock")).unwrap();
    lock.lock().unwrap();
    let output = f.update(&["--version", "v2"], "");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another mintctl operation"));
    assert!(f.commands().is_empty());
}

#[test]
fn invalid_tags_cannot_inject_compose_configuration() {
    let f = Fixture::new(true);
    let before = f.env();
    assert!(!f
        .update(&["--mint-version", "tag\nVERSION=unexpected"], "")
        .status
        .success());
    assert_eq!(before, f.env());
    assert!(f.commands().is_empty());
}

#[test]
fn no_pull_checks_local_images_and_still_waits_for_health() {
    let f = Fixture::new(false);
    success(&f.update(&["--version", "v2", "--no-pull"], ""));
    let commands = f.commands();
    assert!(!commands
        .iter()
        .any(|c| c["args"].as_array().unwrap().iter().any(|a| a == "pull")));
    assert!(commands
        .iter()
        .any(|c| c["args"].as_array().unwrap().iter().any(|a| a == "--wait")));
}

#[test]
fn bootstrap_accepts_equals_pins_and_cleans_up_without_a_controlling_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let stub = temp.path().join("stub");
    fs::write(&stub, "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\"\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("install.sh");
    let output = Command::new("bash")
        .arg(&script)
        .args(["--version=v2", "--yes"])
        .env("MINTCTL_LOCAL_BIN", &stub)
        .output()
        .unwrap();
    success(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(&lines[1..], &["install", "--version=v2", "--yes"]);
    assert!(!Path::new(lines[0]).exists());
    let output = Command::new("bash")
        .arg(script)
        .args(["update", "--version=v2", "--check"])
        .env("MINTCTL_LOCAL_BIN", &stub)
        .output()
        .unwrap();
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("\nupdate\n--version=v2\n--check\n"));
}

#[test]
fn invalid_compose_configuration_never_reaches_backup_or_activation() {
    let f = Fixture::new(true);
    let before = f.env();
    assert!(!f.update(&["--version", "v2"], "config").status.success());
    assert_eq!(before, f.env());
    assert!(!f.install.join("updates").exists());
}

#[test]
fn with_mint_applies_release_configuration_even_when_its_image_pin_is_unchanged() {
    let f = Fixture::new(true);
    fs::write(
        f.install.join(".env"),
        f.env().replace("mint-old", "mint-tested"),
    )
    .unwrap();
    success(&f.update(&["--version", "v2", "--with-mint"], ""));
    let commands = f.commands();
    let up = commands
        .iter()
        .find(|c| {
            c["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "--force-recreate")
        })
        .unwrap();
    for service in ["processor", "mintd"] {
        assert!(up["args"].as_array().unwrap().iter().any(|a| a == service));
    }
}
