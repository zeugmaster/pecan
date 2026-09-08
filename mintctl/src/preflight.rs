//! Host checks the wizard runs before asking anything.

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortStatus {
    Free,
    Busy,
    /// Could not tell (e.g. binding a privileged port without root).
    Unknown,
}

/// Whether something on this host already listens on `port`. A connect probe
/// catches loopback-reachable listeners without needing root; the bind probe
/// then distinguishes free from bound-elsewhere.
pub fn port_status(port: u16) -> PortStatus {
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    if TcpStream::connect_timeout(&loopback, Duration::from_millis(400)).is_ok() {
        return PortStatus::Busy;
    }
    match TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))) {
        Ok(listener) => {
            drop(listener);
            PortStatus::Free
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => PortStatus::Busy,
        // EACCES on 80/443 without root, or anything else exotic.
        Err(_) => PortStatus::Unknown,
    }
}

pub fn ports_80_443_busy() -> bool {
    port_status(80) == PortStatus::Busy || port_status(443) == PortStatus::Busy
}

/// Rootless Docker publishes through the user's host namespace. Report a
/// privileged-port limitation before downloading images or committing secrets.
/// Capabilities granted to rootlesskit can also permit these ports, so the
/// sysctl alone is advisory rather than a reason to reject a working setup.
pub fn check_rootless_ports(ports: &[u16]) -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let output = std::process::Command::new("docker")
        .args(["info", "--format", "{{json .SecurityOptions}}"])
        .output()?;
    if output.status.success() && String::from_utf8_lossy(&output.stdout).contains("rootless") {
        let minimum: u16 = std::fs::read_to_string("/proc/sys/net/ipv4/ip_unprivileged_port_start")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(1024);
        if let Some(port) = ports.iter().find(|&&port| port < minimum) {
            crate::ui::warn(format!("rootless Docker may need additional host configuration to publish port {port}. Use --behind-proxy with an existing proxy, or --plain-http with ports >= {minimum}, unless privileged ports are already enabled for rootlesskit. See https://docs.docker.com/engine/security/rootless/tips/#exposing-privileged-ports"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert_eq!(port_status(port), PortStatus::Busy);
        drop(listener);
        assert_eq!(port_status(port), PortStatus::Free);
    }
}
