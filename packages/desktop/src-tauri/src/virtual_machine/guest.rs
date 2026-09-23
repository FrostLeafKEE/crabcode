use super::*;
use crate::computer_use;
use std::io::{BufRead, BufReader};
use std::os::unix::{fs::PermissionsExt, net::UnixListener};
use std::sync::{Arc, Mutex};

const LABEL: &str = "io.crabcode.computer-use-guest";
const SOCKET: &str = "Library/Application Support/CrabComputerUse/control.sock";

fn virtual_mac() -> bool {
    Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.model"])
        .output()
        .map(|v| {
            v.status.success()
                && String::from_utf8_lossy(&v.stdout)
                    .trim()
                    .starts_with("VirtualMac")
        })
        .unwrap_or(false)
}

#[derive(Default)]
struct Lease {
    owner: Option<String>,
    paused: bool,
    generation: u64,
    observed: bool,
}
impl Lease {
    fn acquire(&mut self, owner: &str) -> Result<(), String> {
        if self.paused {
            return Err("VM is paused for manual control. Reconnect after finishing.".into());
        }
        if owner.is_empty() || owner.len() > 600 {
            return Err("Missing VM session owner".into());
        }
        if self
            .owner
            .as_deref()
            .is_some_and(|current| current != owner)
        {
            return Err(
                "This VM is in use by another session. Wait for that task to finish.".into(),
            );
        }
        if self.owner.is_none() {
            self.observed = false;
        }
        self.owner = Some(owner.into());
        Ok(())
    }
    fn release(&mut self, owner: &str, all_agents: bool) {
        let matches = self.owner.as_deref().is_some_and(|current| {
            current == owner || (all_agents && current.starts_with(&format!("{owner}:")))
        });
        if matches {
            self.owner = None;
            self.observed = false;
        }
    }
}

fn no_input(error: &str) -> Value {
    json!({"ok":false,"error":error,"summary":error,"action_dispatched":false,"retry_safe":true,"effect_verified":false,"focus_isolation":"preserved"})
}

fn handle(message: Value, instance: &str, lease: &mut Lease) -> Value {
    let generation_id = if lease.generation == 0 {
        instance.to_string()
    } else {
        format!("{instance}-{}", lease.generation)
    };
    let instance = generation_id.as_str();
    match message["method"].as_str().unwrap_or("") {
        "capabilities" => {
            let mut value = serde_json::to_value(computer_use::detect_capabilities())
                .unwrap_or(json!({"gui_available":false}));
            value["isolated_vm"] = json!(true);
            value["instance_id"] = json!(instance);
            value["paused"] = json!(lease.paused);
            if lease.paused {
                value["gui_available"] = json!(false);
                value["reason"] = json!("VM is paused for manual control; use Resume automation.");
            }
            value
        }
        "release" => {
            if message["instance_id"] == instance {
                lease.release(
                    message["owner"].as_str().unwrap_or(""),
                    message["all_agents"] == true,
                );
            }
            json!({"ok":true})
        }
        "takeover" => {
            lease.paused = true;
            lease.observed = false;
            lease.generation += 1;
            json!({"ok":true})
        }
        "resume" => {
            lease.paused = false;
            lease.owner = None;
            lease.observed = false;
            lease.generation += 1;
            json!({"ok":true})
        }
        "permissions" => {
            unsafe {
                CGRequestScreenCaptureAccess();
            }
            // Enigo probes Accessibility with the guest app as responsible process.
            let _ = computer_use::detect_capabilities();
            let _ = Command::new("/usr/bin/open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .spawn();
            json!({"ok":true,"summary":"Grant Accessibility and Screen Recording to Crab Computer Use inside this VM, then restart its executor."})
        }
        "execute" => {
            if message["instance_id"] != instance {
                return no_input("VM executor restarted. Refresh its connection and observe again before acting.");
            }
            let request = &message["request"];
            if request["target_scope"] != "desktop"
                || request["delivery_policy"] != "allow_foreground"
            {
                return no_input("VM input must target its own complete desktop");
            }
            let parsed =
                match serde_json::from_value::<computer_use::ExecuteRequest>(request.clone()) {
                    Ok(value) => value,
                    Err(error) => return no_input(&format!("Invalid VM action: {error}")),
                };
            if let Err(error) = lease.acquire(message["owner"].as_str().unwrap_or("")) {
                return no_input(&error);
            }
            let observing = matches!(
                request["action"]["action"].as_str(),
                Some("observe" | "list_displays" | "list_windows" | "wait")
            );
            if !observing && !lease.observed {
                return no_input("Observe this VM's desktop before sending input in a new task or after a restart.");
            }
            match computer_use::execute(parsed) {
                Ok(mut result) => {
                    if result["ok"] == true && result["screenshot"].is_object() {
                        lease.observed = true;
                    }
                    // This is host focus isolation, independent of guest focus changes.
                    result["focus_isolation"] = json!("preserved");
                    result["instance_id"] = json!(instance);
                    result
                }
                Err(error) => {
                    json!({"ok":false,"error":error,"action_dispatched":null,"retry_safe":false,"focus_isolation":"preserved"})
                }
            }
        }
        _ => no_input("Unknown guest operation"),
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGRequestScreenCaptureAccess() -> bool;
}

pub(super) fn run() -> Result<(), String> {
    // This binary is also the host's Desktop binary. Refuse to start the input
    // service there, even if somebody accidentally supplies the guest flag.
    if !virtual_mac() {
        return Err("The guest executor only runs inside an Apple Virtualization macOS VM".into());
    }
    let home = dirs::home_dir().ok_or("Missing guest home")?;
    let socket = home.join(SOCKET);
    let parent = socket.parent().ok_or("Missing socket directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    // launchd owns a single process for this label; stale sockets can survive reboot.
    if socket.exists() {
        std::fs::remove_file(&socket).map_err(|e| e.to_string())?;
    }
    let listener = UnixListener::bind(&socket).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    let instance = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let lease = Arc::new(Mutex::new(Lease::default()));
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
            let mut line = Vec::new();
            let read = BufReader::new((&stream).take(1024 * 1024 + 1)).read_until(b'\n', &mut line);
            if read.is_err() || line.len() > 1024 * 1024 || line.last() != Some(&b'\n') {
                continue;
            }
            let result = match serde_json::from_slice(&line) {
                Ok(message) => handle(
                    message,
                    &instance,
                    &mut lease.lock().unwrap_or_else(|e| e.into_inner()),
                ),
                Err(_) => no_input("Invalid JSON"),
            };
            let _ = serde_json::to_writer(&mut stream, &result);
            let _ = stream.write_all(b"\n");
        }
    });
    // Keep AppKit's main run loop alive for screen capture and macOS consent UI.
    let mtm = objc2::MainThreadMarker::new().ok_or("Guest must start on the main thread")?;
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    app.run();
    Ok(())
}

pub(super) fn install(config: &VmConfig, password: Option<&str>) -> Result<(), String> {
    let ip = guest_address(&vm_info(config)?)?;
    let state = state_dir()?;
    let identity = state.join("id_ed25519");
    if !identity.exists() {
        let mut keygen = Command::new("/usr/bin/ssh-keygen");
        keygen
            .args(["-t", "ed25519", "-N", "", "-C", "Crab local VM"])
            .arg("-f")
            .arg(&identity);
        output(keygen, vec![], Duration::from_secs(15))?;
    }
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    if let Some(password) = password.filter(|p| !p.is_empty()) {
        let passfile = temp.path().join("password");
        std::fs::write(&passfile, password).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&passfile, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        let askpass = temp.path().join("askpass");
        std::fs::write(
            &askpass,
            "#!/bin/sh\nexec /bin/cat \"$CRAB_VM_PASSWORD_FILE\"\n",
        )
        .map_err(|e| e.to_string())?;
        std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let public_key =
            std::fs::read_to_string(identity.with_extension("pub")).map_err(|e| e.to_string())?;
        // Build fresh arguments so BatchMode=no precedes no conflicting option.
        let mut c = Command::new("/usr/bin/ssh");
        c.args(["-F", "/dev/null", "-o", "PubkeyAuthentication=no"]);
        c.args([
            "-T",
            "-o",
            "BatchMode=no",
            "-o",
            "NumberOfPasswordPrompts=1",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "StrictHostKeyChecking=accept-new",
        ]);
        c.arg("-o").arg(format!(
            "UserKnownHostsFile={}",
            state.join("known_hosts").display()
        ));
        c.arg("-o").arg(format!(
            "HostKeyAlias=crab-{}-{}",
            config.storage, config.name
        ));
        c.arg(format!("{}@{ip}", config.user));
        // Guard the installation itself as well as the service startup.
        c.arg(format!("case $(/usr/sbin/sysctl -n hw.model) in VirtualMac*) ;; *) exit 42;; esac; umask 077; mkdir -p \"$HOME/.ssh\"; touch \"$HOME/.ssh/authorized_keys\"; /usr/bin/grep -qxF {key} \"$HOME/.ssh/authorized_keys\" || /usr/bin/printf '%s\\n' {key} >> \"$HOME/.ssh/authorized_keys\"", key=quote(public_key.trim())));
        c.env("SSH_ASKPASS", &askpass)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", ":0")
            .env("CRAB_VM_PASSWORD_FILE", &passfile);
        output(c, vec![], Duration::from_secs(25))?;
        std::fs::remove_file(passfile).map_err(|e| e.to_string())?;
    }
    let app = temp.path().join(APP);
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).map_err(|e| e.to_string())?;
    std::fs::copy(
        std::env::current_exe().map_err(|e| e.to_string())?,
        macos.join("crab-guest"),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(app.join("Contents/Info.plist"), format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{LABEL}</string><key>CFBundleName</key><string>Crab Computer Use</string><key>CFBundleExecutable</key><string>crab-guest</string><key>CFBundlePackageType</key><string>APPL</string><key>LSUIElement</key><true/><key>NSHighResolutionCapable</key><true/></dict></plist>"#)).map_err(|e| e.to_string())?;
    let mut sign = Command::new("/usr/bin/codesign");
    sign.args(["--force", "--sign", "-"]).arg(&app);
    output(sign, vec![], Duration::from_secs(30))?;
    let archive = temp.path().join("guest.tar.gz");
    let mut tar = Command::new("/usr/bin/tar");
    tar.arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(temp.path())
        .arg(APP);
    output(tar, vec![], Duration::from_secs(60))?;
    let mut upload = ssh(config, &ip)?;
    upload.arg("case $(/usr/sbin/sysctl -n hw.model) in VirtualMac*) ;; *) exit 42;; esac; uid=$(/usr/bin/id -u); /bin/launchctl bootout \"gui/$uid/io.crabcode.computer-use-guest\" 2>/dev/null || true; mkdir -p \"$HOME/Applications\"; /usr/bin/tar -xzf - -C \"$HOME/Applications\"");
    output(
        upload,
        std::fs::read(archive).map_err(|e| e.to_string())?,
        Duration::from_secs(120),
    )?;
    // Generate the launch agent on the guest, so its actual home path is used.
    let script = format!(
        r#"set -eu
case $(/usr/sbin/sysctl -n hw.model) in VirtualMac*) ;; *) exit 42;; esac
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs/CrabComputerUse"
plist="$HOME/Library/LaunchAgents/{LABEL}.plist"
/usr/bin/osascript -l JavaScript - "$HOME" "$plist" <<'JXA'
ObjC.import('Foundation');
function run(argv) {{
  const home = argv[0];
  const data = {{Label: '{LABEL}', ProgramArguments: [home + '/Applications/{APP}/Contents/MacOS/crab-guest', '--computer-use-guest'], RunAtLoad: true, KeepAlive: true, LimitLoadToSessionType: 'Aqua', StandardOutPath: home + '/Library/Logs/CrabComputerUse/stdout.log', StandardErrorPath: home + '/Library/Logs/CrabComputerUse/stderr.log'}};
  if (!$(data).writeToFileAtomically(argv[1], true)) throw Error('Could not write guest launch agent');
}}
JXA
uid=$(/usr/bin/id -u)
/bin/launchctl bootout "gui/$uid/{LABEL}" 2>/dev/null || true
/bin/launchctl bootstrap "gui/$uid" "$plist"
/bin/launchctl kickstart "gui/$uid/{LABEL}"
"#
    );
    let mut setup = ssh(config, &ip)?;
    setup.arg("/bin/sh -s");
    output(setup, script.into_bytes(), Duration::from_secs(30))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_owner_and_release_boundaries() {
        let mut lease = Lease::default();
        lease.acquire("desktop:s1:a").unwrap();
        assert!(lease.acquire("desktop:s2:a").is_err());
        lease.release("desktop:s", true);
        assert!(lease.owner.is_some());
        lease.release("desktop:s1", true);
        assert!(lease.owner.is_none());
        lease.paused = true;
        assert!(lease.acquire("desktop:s2:a").is_err());
    }
    #[test]
    fn stale_instance_never_reaches_native_input() {
        let value = handle(
            json!({"method":"execute","instance_id":"old","request":{"action":{"action":"click"}}}),
            "new",
            &mut Lease::default(),
        );
        assert_eq!(value["action_dispatched"], false);
        assert!(value["error"].as_str().unwrap().contains("restarted"));
    }
    #[test]
    fn new_task_requires_observation_before_native_input() {
        let mut lease = Lease::default();
        let value = handle(
            json!({"method":"execute","instance_id":"boot","owner":"task", "request":{
                "mode":"foreground_desktop", "target_scope":"desktop", "delivery_policy":"allow_foreground",
                "action":{"action":"click", "x":1,"y":1}
            }}),
            "boot",
            &mut lease,
        );
        assert_eq!(value["action_dispatched"], false);
        assert!(value["error"].as_str().unwrap().contains("Observe"));
    }
    #[test]
    fn manual_takeover_invalidates_previously_queued_input() {
        let mut lease = Lease::default();
        handle(json!({"method":"takeover"}), "boot", &mut lease);
        handle(json!({"method":"resume"}), "boot", &mut lease);
        let value = handle(
            json!({"method":"execute","instance_id":"boot","owner":"task"}),
            "boot",
            &mut lease,
        );
        assert_eq!(value["action_dispatched"], false);
        assert!(value["error"].as_str().unwrap().contains("restarted"));
    }
    #[test]
    fn invalid_scope_never_reaches_native_input() {
        let value = handle(
            json!({"method":"execute","instance_id":"current","request":{"target_scope":"app_window"}}),
            "current",
            &mut Lease::default(),
        );
        assert_eq!(value["action_dispatched"], false);
    }
}
