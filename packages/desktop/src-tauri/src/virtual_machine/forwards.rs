//! Explicit localhost-only guest -> host TCP forwarding, owned by Desktop.
use super::*;
use std::collections::HashMap;
use std::process::Child;
use std::sync::{Mutex, OnceLock};

struct Tunnel {
    child: Child,
    address: String,
    ports: Vec<u16>,
    user: String,
    _directory: tempfile::TempDir,
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
static TUNNELS: OnceLock<Mutex<HashMap<String, Tunnel>>> = OnceLock::new();

pub(super) fn stop_all() {
    if let Some(tunnels) = TUNNELS.get() {
        if let Ok(mut tunnels) = tunnels.lock() {
            tunnels.clear();
        }
    }
}

pub(super) fn stop(config: &VmConfig) {
    if let Some(tunnels) = TUNNELS.get() {
        if let Ok(mut tunnels) = tunnels.lock() {
            tunnels.remove(&config.environment_id());
        }
    }
}

pub(super) fn ensure(config: &VmConfig) -> Result<(), String> {
    config.validate()?;
    if config.forwarded_ports.is_empty() {
        stop(config);
        return Ok(());
    }
    let address = guest_address(&vm_info(config)?)?;
    let key = config.environment_id();
    let mut tunnels = TUNNELS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "VM port forwarding lock failed")?;
    if let Some(tunnel) = tunnels.get_mut(&key) {
        if tunnel.address == address
            && tunnel.user == config.user
            && tunnel.ports == config.forwarded_ports
            && tunnel
                .child
                .try_wait()
                .map_err(|e| e.to_string())?
                .is_none()
        {
            return Ok(());
        }
    }
    tunnels.remove(&key);
    if config.forwarded_ports.is_empty() {
        return Ok(());
    }
    let directory = tempfile::Builder::new()
        .prefix("crab-vm-")
        .tempdir_in("/tmp")
        .map_err(|e| e.to_string())?;
    let control = directory.path().join("ssh");
    let error_path = directory.path().join("error");
    let mut c = ssh_options(config)?;
    c.args([
        "-N",
        "-M",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ControlPersist=no",
    ])
    .arg("-S")
    .arg(&control);
    for port in &config.forwarded_ports {
        c.arg("-R")
            .arg(format!("127.0.0.1:{port}:127.0.0.1:{port}"));
    }
    c.arg(format!("{}@{}", config.user, address));
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&error_path).map_err(|e| e.to_string())?);
    let mut tunnel = Tunnel {
        child: c.spawn().map_err(|e| e.to_string())?,
        address,
        user: config.user.clone(),
        ports: config.forwarded_ports.clone(),
        _directory: directory,
    };
    let start = Instant::now();
    loop {
        if tunnel
            .child
            .try_wait()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            let error = std::fs::read_to_string(&error_path).unwrap_or_default();
            return Err(format!(
                "Could not forward host ports: {}",
                error.chars().take(1200).collect::<String>()
            ));
        }
        if control.exists() {
            break;
        }
        if start.elapsed() > Duration::from_secs(12) {
            return Err("Timed out connecting host port forwarding".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    tunnels.insert(key, tunnel);
    Ok(())
}
