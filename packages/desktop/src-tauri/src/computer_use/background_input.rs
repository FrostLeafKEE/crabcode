//! Strict-background input owns preparation, routing, dispatch and cleanup.
//! Private ABI reference: trycua/cua 9bbfa7dd3e27ca7f1861ede70aaca390174493f9,
//! platform-macos/src/input/{skylight,mouse}.rs. We deliberately never defocus
//! the user's application, raise a window, restore global focus, or double-post.
//! A resolved symbol is not a compatibility certificate. Production dispatch
//! requires an exact, checked-in OS/app/action validation profile.
use super::*;
use core_foundation::runloop::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRemoveSource,
    CFRunLoopRunInMode, CFRunLoopSourceRef,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};

#[cfg(test)]
mod tests;

type PostRecord = unsafe extern "C" fn(*const u32, *const u8) -> i32;
type PostEvent = unsafe extern "C" fn(i32, core_graphics::sys::CGEventRef);
type Connection = unsafe extern "C" fn() -> i32;
type DisplaySpaces = unsafe extern "C" fn(i32) -> CFArrayRef;
type ProcessPid = unsafe extern "C" fn(*const u32, *mut i32) -> i32;

unsafe extern "C" {
    fn CGEventSetTimestamp(event: core_graphics::sys::CGEventRef, timestamp: u64);
    fn clock_gettime_nsec_np(clock_id: libc::clockid_t) -> u64;
    fn AXObserverCreateWithInfoCallback(
        pid: i32,
        callback: unsafe extern "C" fn(
            CFTypeRef,
            CFTypeRef,
            CFStringRef,
            CFDictionaryRef,
            *mut libc::c_void,
        ),
        observer: *mut CFTypeRef,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: CFTypeRef) -> CFRunLoopSourceRef;
    fn AXObserverAddNotification(
        observer: CFTypeRef,
        element: CFTypeRef,
        notification: CFStringRef,
        context: *mut libc::c_void,
    ) -> i32;
    fn AXObserverRemoveNotification(
        observer: CFTypeRef,
        element: CFTypeRef,
        notification: CFStringRef,
    ) -> i32;
}

fn attribute(element: &CFType, name: &str) -> Result<CFType, String> {
    mac_ax_copy_attribute(element, name).map_err(|status| mac_ax_error(name, status))
}

pub(super) fn enable_accessibility(application: &CFType) {
    for name in ["AXManualAccessibility", "AXEnhancedUserInterface"] {
        if mac_ax_copy_attribute(application, name)
            .ok()
            .and_then(|v| v.downcast::<CFBoolean>())
            .is_some_and(bool::from)
        {
            return;
        }
        let attr = CFString::new(name);
        let status = unsafe {
            AXUIElementSetAttributeValue(
                application.as_CFTypeRef(),
                attr.as_concrete_TypeRef(),
                CFBoolean::true_value().as_CFTypeRef(),
            )
        };
        if status == AX_ERROR_SUCCESS {
            thread::sleep(Duration::from_millis(500));
            return;
        }
        // Retry the legacy attribute only for an unsupported modern attribute.
        if status != -25205 {
            return;
        }
    }
}

/// Keep the target's remote AX connection serviced throughout an operation.
/// Registration alone on a spawn_blocking thread leaves its source unserviced.
/// ABI reference: Cua 8a4c51337cfdc91a1818ee2f92ceb427272a6247 AppState.swift.
pub(super) struct AccessibilityLease {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl AccessibilityLease {
    pub(super) fn start(pid: i32) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            unsafe extern "C" fn callback(
                _: CFTypeRef,
                _: CFTypeRef,
                _: CFStringRef,
                _: CFDictionaryRef,
                _: *mut libc::c_void,
            ) {
            }
            type Add =
                unsafe extern "C" fn(CFTypeRef, CFTypeRef, CFStringRef, *mut libc::c_void) -> i32;
            let application = match mac_ax_relations::application(pid) {
                Ok(app) => app,
                Err(reason) => {
                    let _ = ready_tx.send(Err(reason));
                    return;
                }
            };
            enable_accessibility(&application);
            let mut observer = std::ptr::null();
            let status = unsafe { AXObserverCreateWithInfoCallback(pid, callback, &mut observer) };
            if status != 0 || observer.is_null() {
                let _ = ready_tx.send(Err(mac_ax_error("Creating target AX observer", status)));
                return;
            }
            let observer = unsafe { CFType::wrap_under_create_rule(observer) };
            let source = unsafe { AXObserverGetRunLoopSource(observer.as_CFTypeRef()) };
            if source.is_null() {
                let _ = ready_tx.send(Err("Target AX observer has no run-loop source".into()));
                return;
            }
            let remote = unsafe {
                libc::dlsym(
                    libc::RTLD_DEFAULT,
                    c"AXObserverAddNotificationAndCheckRemote".as_ptr(),
                )
            };
            let add: Add = if remote.is_null() {
                AXObserverAddNotification
            } else {
                unsafe { std::mem::transmute::<*mut libc::c_void, Add>(remote) }
            };
            let mut notifications = Vec::new();
            for name in [
                "AXFocusedUIElementChanged",
                "AXFocusedWindowChanged",
                "AXApplicationActivated",
                "AXApplicationDeactivated",
                "AXWindowCreated",
                "AXValueChanged",
                "AXTitleChanged",
                "AXSelectedChildrenChanged",
                "AXLayoutChanged",
            ] {
                let name = CFString::new(name);
                if unsafe {
                    add(
                        observer.as_CFTypeRef(),
                        application.as_CFTypeRef(),
                        name.as_concrete_TypeRef(),
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    notifications.push(name);
                }
            }
            if notifications.is_empty() {
                let _ = ready_tx.send(Err(
                    "Target did not accept any AX observer notification".into()
                ));
                return;
            }
            unsafe {
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
            }
            let settle = std::time::Instant::now();
            while settle.elapsed() < Duration::from_millis(500)
                && !worker_stop.load(Ordering::Acquire)
            {
                unsafe {
                    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.025, 0);
                }
            }
            let _ = ready_tx.send(Ok(()));
            while !worker_stop.load(Ordering::Acquire) {
                unsafe {
                    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.025, 0);
                }
            }
            unsafe {
                CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
                for name in notifications {
                    AXObserverRemoveNotification(
                        observer.as_CFTypeRef(),
                        application.as_CFTypeRef(),
                        name.as_concrete_TypeRef(),
                    );
                }
            }
        });
        let lease = Self {
            stop,
            worker: Some(worker),
        };
        ready_rx
            .recv()
            .map_err(|_| "Target AX observer failed to start")??;
        Ok(lease)
    }
}

impl Drop for AccessibilityLease {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Api {
    record: PostRecord,
    post: PostEvent,
    connection: Connection,
    spaces: DisplaySpaces,
    process_pid: ProcessPid,
}

impl Api {
    fn load() -> Result<&'static Self, String> {
        static API: OnceLock<Result<Api, String>> = OnceLock::new();
        API.get_or_init(|| unsafe {
            let handle = libc::dlopen(
                c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight".as_ptr(),
                libc::RTLD_LAZY | libc::RTLD_LOCAL,
            );
            if handle.is_null() {
                return Err("SkyLight framework is unavailable".into());
            }
            // Keep this system framework loaded for the lifetime of the pointers.
            macro_rules! symbol {
                ($name:expr, $ty:ty) => {{
                    let pointer = libc::dlsym(handle, $name.as_ptr());
                    if pointer.is_null() {
                        return Err(format!(
                            "Required SkyLight symbol {} is unavailable",
                            $name.to_string_lossy()
                        ));
                    }
                    std::mem::transmute::<*mut libc::c_void, $ty>(pointer)
                }};
            }
            let pid = libc::dlsym(libc::RTLD_DEFAULT, c"GetProcessPID".as_ptr());
            if pid.is_null() {
                return Err("GetProcessPID is unavailable".into());
            }
            Ok(Api {
                record: symbol!(c"SLPSPostEventRecordTo", PostRecord),
                post: symbol!(c"SLEventPostToPid", PostEvent),
                connection: symbol!(c"SLSMainConnectionID", Connection),
                spaces: symbol!(c"SLSCopyManagedDisplaySpaces", DisplaySpaces),
                process_pid: std::mem::transmute::<*mut libc::c_void, ProcessPid>(pid),
            })
        })
        .as_ref()
        .map_err(Clone::clone)
    }

    fn spaces(&self) -> Result<Vec<(String, i64)>, String> {
        let raw = unsafe { (self.spaces)((self.connection)()) };
        if raw.is_null() {
            return Err("Unable to observe active Spaces".into());
        }
        let array = unsafe { CFArray::<CFType>::wrap_under_create_rule(raw) };
        let mut spaces = Vec::new();
        for value in array.iter() {
            let dict = value
                .downcast::<CFDictionary>()
                .ok_or("Invalid display Space record")?;
            let display = mac_dictionary_string(&dict, "Display Identifier")
                .ok_or("Missing display identity")?;
            let current = mac_dictionary_value(&dict, "Current Space")
                .and_then(|v| v.downcast::<CFDictionary>())
                .ok_or("Missing current Space")?;
            let id = mac_dictionary_number(&current, "ManagedSpaceID")
                .ok_or("Missing Space identity")?;
            spaces.push((display, id));
        }
        if spaces.is_empty() {
            return Err("No active display Space could be observed".into());
        }
        spaces.sort();
        Ok(spaces)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    os_build: String,
    architecture: String,
    bundle_id: String,
    app_version: String,
    app_build: String,
}

fn os_build() -> Result<String, String> {
    static BUILD: OnceLock<Result<String, String>> = OnceLock::new();
    BUILD
        .get_or_init(|| {
            let mut bytes = [0u8; 256];
            let mut len = bytes.len();
            let status = unsafe {
                libc::sysctlbyname(
                    c"kern.osversion".as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if status != 0 || len == 0 || len > bytes.len() {
                return Err("Unable to identify this macOS build".into());
            }
            Ok(String::from_utf8_lossy(&bytes[..len - 1]).into_owned())
        })
        .clone()
}

fn identity(pid: i32) -> Result<Identity, String> {
    let app = objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .ok_or("Target application no longer exists")?;
    let architecture = match app.executableArchitecture() {
        0x0100_000c => "aarch64",
        0x0100_0007 => "x86_64",
        _ => return Err("Target executable architecture has not been validated".into()),
    };
    if architecture != std::env::consts::ARCH {
        return Err("Cross-architecture background input has not been validated".into());
    }
    let bundle_id = app
        .bundleIdentifier()
        .ok_or("Target has no bundle identity")?
        .to_string();
    let path = app
        .bundleURL()
        .and_then(|url| url.path())
        .ok_or("Target has no bundle path")?
        .to_string();
    let plist = std::path::Path::new(&path).join("Contents/Info.plist");
    let launched = app
        .launchDate()
        .ok_or("Cannot identify target process launch time")?
        .timeIntervalSince1970();
    let modified = std::fs::metadata(&plist)
        .and_then(|m| m.modified())
        .map_err(|e| e.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs_f64();
    if modified > launched {
        return Err("The application bundle changed after this process launched; its running version is unverified. Reopen the application before background validation".into());
    }
    let value = |key: &str| -> Result<String, String> {
        let output = Command::new("/usr/bin/plutil")
            .args(["-extract", key, "raw", "-o", "-"])
            .arg(&plist)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!("Target bundle is missing {key}"));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    Ok(Identity {
        os_build: os_build()?,
        architecture: architecture.into(),
        bundle_id,
        app_version: value("CFBundleShortVersionString")?,
        app_build: value("CFBundleVersion")?,
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Route {
    Quartz,
    SkyLight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Profile {
    identity: Identity,
    actions: Vec<String>,
    #[serde(default)]
    shortcuts: Vec<Vec<String>>,
    pointer: Route,
    #[serde(default)]
    scroll: Option<Route>,
    keyboard: Route,
    prepare_active: bool,
    primer: bool,
    semantic_click: bool,
    #[serde(default)]
    ax_actions: Vec<AxRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct AxRule {
    pub role: String,
    pub subrole: Option<String>,
    // Exact advertised AX action, or AXValue for a writable text field.
    pub operation: String,
}

pub(super) fn ax_rules(target: WindowTarget) -> Vec<AxRule> {
    identity(target.pid)
        .ok()
        .and_then(|id| profiles().iter().find(|p| p.identity == id))
        .map(|p| p.ax_actions.clone())
        .unwrap_or_default()
}

pub(super) fn ax_allowed(
    rules: &[AxRule],
    role: &str,
    subrole: Option<&str>,
    operation: &str,
) -> bool {
    rules.iter().any(|rule| {
        rule.role == role && rule.subrole.as_deref() == subrole && rule.operation == operation
    })
}

/// Semantic input does not prepare/activate a process or synthesize mouse events.
/// It uses the same focus, cursor, Space and window-order observation as the
/// validated event provider. Profile eligibility is checked before starting it.
pub(super) struct AxIsolation {
    monitor: IsolationMonitor,
    _accessibility: AccessibilityLease,
}

impl AxIsolation {
    pub(super) fn start(target: WindowTarget) -> Result<Self, String> {
        let monitor = IsolationMonitor::start(target)?;
        let accessibility = AccessibilityLease::start(target.pid)?;
        monitor.check()?;
        Ok(Self {
            monitor,
            _accessibility: accessibility,
        })
    }

    pub(super) fn check(&self) -> Result<(), String> {
        self.monitor.check()
    }

    pub(super) fn finish(&mut self, result: &mut Value) {
        // Retain monitoring through the post-action AX observation and delayed UI work.
        thread::sleep(Duration::from_millis(500));
        let isolation = self.monitor.finish();
        result["focus_isolation"] = json!(if isolation.is_ok() {
            "preserved"
        } else {
            "violated"
        });
        result["isolation_verification"] = json!(
            "sampled_front_process_cursor_all_display_spaces_window_order_and_foreground_ax_focus"
        );
        result["isolation_samples"] = self.monitor.evidence.clone();
        result["cleanup_succeeded"] = json!(true);
        if let Err(reason) = isolation {
            result["ok"] = json!(false);
            result["error_code"] = json!("background_isolation_interrupted");
            result["error"] = json!(reason);
            result["summary"] = json!("Background isolation changed; observe before continuing");
            result["requires_observation"] = json!(true);
            result["retry_safe"] = json!(false);
        }
    }
}

fn profiles() -> &'static [Profile] {
    static PROFILES: OnceLock<Vec<Profile>> = OnceLock::new();
    PROFILES.get_or_init(|| {
        serde_json::from_str(include_str!("background_profiles.json"))
            .expect("checked-in background profiles must be valid JSON")
    })
}

pub(super) fn available() -> bool {
    Api::load().is_ok()
        && os_build().is_ok_and(|build| {
            profiles().iter().any(|p| {
                p.identity.os_build == build && p.identity.architecture == std::env::consts::ARCH
            })
        })
}

fn supported(profile: &Profile, id: &Identity, action: &ComputerAction) -> bool {
    profile.identity == *id
        && profile.actions.contains(&action.action)
        && (!matches!(action.action.as_str(), "click" | "double_click" | "drag")
            || action
                .button
                .as_deref()
                .unwrap_or("left")
                .eq_ignore_ascii_case("left"))
        && (action.action != "keypress"
            || profile.shortcuts.iter().any(|keys| {
                mac_key_combination(keys).ok()
                    == mac_key_combination(action.keys.as_deref().unwrap_or_default()).ok()
                    && mac_key_combination(keys).is_ok()
            }))
        && (action.action != "type"
            || action
                .text
                .as_deref()
                .is_some_and(|text| text.chars().all(|c| c.len_utf16() == 1 && !c.is_control())))
}

fn rejected(action: &ComputerAction, reason: impl Into<String>) -> Value {
    json!({"ok": false, "action": action.action, "error_code": "background_delivery_unsupported",
        "error": reason.into(), "summary": "This OS/app/action combination has not passed strict-background validation; no input was sent",
        "action_dispatched": false, "dispatch_succeeded": false, "effect_verified": false,
        "retry_safe": true, "focus_isolation": "preserved", "input_method": "none",
    })
}

pub(super) fn execute(action: &ComputerAction, target: WindowTarget) -> Value {
    let id = match identity(target.pid) {
        Ok(id) => id,
        Err(reason) => return rejected(action, reason),
    };
    let Some(profile) = profiles().iter().find(|p| p.identity == id) else {
        let mut result = rejected(action, "No validated background provider for this exact macOS build, application version and gesture. Foreground fallback is disabled");
        result["background_target"] = json!(id);
        return result;
    };
    if !supported(profile, &id, action) {
        let mut result = rejected(action, "This action or its parameters are outside the validated profile; consult validated_actions and validated_shortcuts. Foreground fallback is disabled");
        result["validated_actions"] = json!(profile.actions);
        result["validated_shortcuts"] = json!(profile.shortcuts);
        result["background_target"] = json!(id);
        return result;
    }
    run(action, target, profile)
}

fn activation_record(window: u32, active: bool) -> [u8; 248] {
    let mut record = [0; 248];
    record[4] = 248;
    record[8] = 13;
    record[60..64].copy_from_slice(&window.to_le_bytes());
    record[138] = if active { 1 } else { 2 };
    record
}

struct Activation {
    api: &'static Api,
    psn: [u32; 2],
    window: u32,
    armed: bool,
}

impl Activation {
    fn new(api: &'static Api, target: WindowTarget) -> Result<Self, String> {
        let psn = mac_process_serial_number(target.pid)?;
        Ok(Self {
            api,
            psn,
            window: target.window_id,
            armed: false,
        })
    }

    fn prepare(&mut self, target: WindowTarget, enabled: bool) -> Result<(), String> {
        if enabled && mac_front_process_serial_number()? != self.psn {
            let application = mac_ax_relations::application(target.pid)?;
            let active: bool = attribute(&application, "AXFrontmost")?
                .downcast::<CFBoolean>()
                .ok_or("Cannot observe target input state")?
                .into();
            if !active {
                // Arm before calling: cleanup is attempted even after a partial native failure.
                self.armed = true;
                if unsafe {
                    (self.api.record)(
                        self.psn.as_ptr(),
                        activation_record(target.window_id, true).as_ptr(),
                    )
                } != 0
                {
                    return Err("Target input-state preparation failed".into());
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        // If the user selected the target, do not deactivate their new foreground.
        // Never restore a previous global front process or write to another app.
        if mac_front_process_serial_number()? == self.psn {
            self.armed = false;
            return Ok(());
        }
        let status = unsafe {
            (self.api.record)(
                self.psn.as_ptr(),
                activation_record(self.window, false).as_ptr(),
            )
        };
        if status != 0 {
            return Err("Target input-state cleanup failed".into());
        }
        self.armed = false;
        thread::sleep(Duration::from_millis(30));
        let mut pid = 0;
        if unsafe { (self.api.process_pid)(self.psn.as_ptr(), &mut pid) } != 0 {
            return Err("Cannot verify target input-state cleanup".into());
        }
        let application = mac_ax_relations::application(pid)?;
        let active = attribute(&application, "AXFrontmost")?
            .downcast::<CFBoolean>()
            .ok_or("Cannot verify target input-state cleanup")?;
        if bool::from(active) {
            return Err("Target still reports active input state after cleanup".into());
        }
        Ok(())
    }
}

impl Drop for Activation {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

struct IsolationBaseline {
    front: [u32; 2],
    cursor: CGPoint,
    spaces: Vec<(String, i64)>,
    foreground: CFType,
    focused: CFType,
    above_target: Vec<u32>,
    above_existing: std::collections::BTreeMap<u32, Vec<u32>>,
    below_existing: std::collections::BTreeMap<u32, Vec<u32>>,
    target: WindowTarget,
}

impl IsolationBaseline {
    fn capture(api: &Api, target: WindowTarget) -> Result<Self, String> {
        let front = mac_front_process_serial_number()?;
        let mut pid = 0;
        if unsafe { (api.process_pid)(front.as_ptr(), &mut pid) } != 0 {
            return Err("Cannot identify foreground application".into());
        }
        if pid == target.pid {
            return Err(
                "Strict-background validation requires the target to be behind another application"
                    .into(),
            );
        }
        let foreground = mac_ax_relations::application(pid)?;
        let focused = attribute(&foreground, "AXFocusedUIElement")?;
        if mac_ax_string(&focused, "AXRole")
            .as_deref()
            .is_none_or(|role| role == "AXApplication")
        {
            return Err("The foreground application's actual focused element is unavailable; isolation cannot be verified".into());
        }
        let windows = mac_all_window_info()?;
        let index = windows
            .iter()
            .position(|w| {
                w.target.window_id == target.window_id && w.target.pid == target.pid && w.on_screen
            })
            .ok_or(
                "Hidden, minimized and other-Space targets have not passed background validation",
            )?;
        let above_existing = windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.on_screen && w.target.pid == target.pid)
            .map(|(index, w)| {
                (
                    w.target.window_id,
                    windows[..index]
                        .iter()
                        .filter(|other| {
                            other.on_screen && other.layer == 0 && other.target.pid != target.pid
                        })
                        .map(|other| other.target.window_id)
                        .collect(),
                )
            })
            .collect();
        let below_existing = windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.on_screen && w.target.pid == target.pid)
            .map(|(index, w)| {
                (
                    w.target.window_id,
                    windows[index + 1..]
                        .iter()
                        .filter(|other| {
                            other.on_screen && other.layer == 0 && other.target.pid != target.pid
                        })
                        .map(|other| other.target.window_id)
                        .collect(),
                )
            })
            .collect();
        Ok(Self {
            front,
            cursor: CGEvent::new(mac_event_source()?)
                .map_err(|_| "Cannot read cursor")?
                .location(),
            spaces: api.spaces()?,
            foreground,
            focused,
            above_target: windows[..index]
                .iter()
                .filter(|w| w.on_screen && w.layer == 0 && w.target.pid != target.pid)
                .map(|w| w.target.window_id)
                .collect(),
            above_existing,
            below_existing,
            target,
        })
    }

    fn check(&self, api: &Api, full: bool) -> Result<(), String> {
        if mac_front_process_serial_number()? != self.front {
            return Err("Foreground application changed during background input".into());
        }
        let cursor = CGEvent::new(mac_event_source()?)
            .map_err(|_| "Cannot read cursor")?
            .location();
        if cursor.x != self.cursor.x || cursor.y != self.cursor.y {
            return Err("Hardware cursor changed during background input; user activity or isolation loss detected".into());
        }
        if api.spaces()? != self.spaces {
            return Err("Active display Space changed during background input".into());
        }
        if full {
            if attribute(&self.foreground, "AXFocusedUIElement")? != self.focused {
                return Err("The user's focused UI element changed during background input".into());
            }
            let active = attribute(&self.foreground, "AXFrontmost")?
                .downcast::<CFBoolean>()
                .ok_or("Cannot verify foreground input state")?;
            if !bool::from(active) {
                return Err("The user's application lost its active input state".into());
            }
            let windows = mac_all_window_info()?;
            // Input may legitimately close or resize its target. Geometry is
            // checked before a gesture, not classified as focus loss.
            // New dialogs/popovers must also stay behind the user's windows.
            // Watching only the original window misses a raised auxiliary surface.
            check_window_order(
                &windows,
                self.target.pid,
                &self.above_target,
                &self.above_existing,
                &self.below_existing,
            )?;
        }
        Ok(())
    }
}

fn check_window_order(
    windows: &[MacWindowInfo],
    pid: i32,
    above_target: &[u32],
    above_existing: &std::collections::BTreeMap<u32, Vec<u32>>,
    below_existing: &std::collections::BTreeMap<u32, Vec<u32>>,
) -> Result<(), String> {
    for (index, window) in windows
        .iter()
        .enumerate()
        .filter(|(_, w)| w.on_screen && w.target.pid == pid)
    {
        let protected = above_existing
            .get(&window.target.window_id)
            .map(Vec::as_slice)
            .unwrap_or(above_target);
        if windows[index + 1..]
            .iter()
            .any(|w| w.on_screen && protected.contains(&w.target.window_id))
        {
            return Err(format!(
                "Target window {} rose above a protected user window during background input",
                window.target.window_id
            ));
        }
        if below_existing
            .get(&window.target.window_id)
            .is_some_and(|below| {
                windows[..index]
                    .iter()
                    .any(|w| w.on_screen && below.contains(&w.target.window_id))
            })
        {
            return Err(format!("Target window {} changed its order relative to another application during background input", window.target.window_id));
        }
    }
    Ok(())
}

struct IsolationMonitor {
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    worker: Option<thread::JoinHandle<Value>>,
    evidence: Value,
}

impl IsolationMonitor {
    fn start(target: WindowTarget) -> Result<Self, String> {
        let api = Api::load()?;
        let stop = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker_stop = stop.clone();
        let worker_failure = failure.clone();
        let worker = thread::spawn(move || {
            let baseline = match IsolationBaseline::capture(api, target) {
                Ok(value) => value,
                Err(reason) => {
                    let _ = ready_tx.send(Err(reason));
                    return Value::Null;
                }
            };
            if let Err(reason) = baseline.check(api, true) {
                let _ = ready_tx.send(Err(reason));
                return Value::Null;
            }
            let _ = ready_tx.send(Ok(()));
            let mut count = 0;
            let start = std::time::Instant::now();
            let mut previous = start;
            let mut max_gap = 0.0_f64;
            loop {
                let now = std::time::Instant::now();
                max_gap = max_gap.max(now.duration_since(previous).as_secs_f64() * 1000.0);
                previous = now;
                let stopping = worker_stop.load(Ordering::Acquire);
                if let Err(reason) = baseline.check(api, stopping || count % 5 == 0) {
                    *worker_failure.lock().unwrap() = Some(reason);
                    break;
                }
                if stopping {
                    break;
                }
                count += 1;
                thread::sleep(Duration::from_millis(2));
            }
            json!({"samples": count + 1, "duration_ms":start.elapsed().as_secs_f64()*1000.0,
                "max_sample_gap_ms":max_gap,"foreground_psn":baseline.front,
                "cursor":[baseline.cursor.x,baseline.cursor.y],"display_spaces":baseline.spaces,
                "protected_window_count":baseline.above_target.len()})
        });
        let monitor = Self {
            stop,
            failure,
            worker: Some(worker),
            evidence: Value::Null,
        };
        ready_rx
            .recv()
            .map_err(|_| "Isolation monitor failed to start")??;
        Ok(monitor)
    }

    fn check(&self) -> Result<(), String> {
        match self.failure.lock().unwrap().clone() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    fn finish(&mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            self.evidence = worker.join().map_err(|_| "Isolation monitor failed")?;
        }
        self.check()
    }
}

impl Drop for IsolationMonitor {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

struct Session<'a> {
    api: &'static Api,
    monitor: IsolationMonitor,
    activation: Option<Activation>,
    _accessibility: AccessibilityLease,
    target: WindowTarget,
    profile: &'a Profile,
    events: usize,
    group: i64,
}

impl<'a> Session<'a> {
    fn start(target: WindowTarget, profile: &'a Profile) -> Result<Self, String> {
        let api = Api::load()?;
        let monitor = IsolationMonitor::start(target)?;
        let accessibility = AccessibilityLease::start(target.pid)?;
        monitor.check()?;
        Ok(Self {
            api,
            monitor,
            activation: None,
            _accessibility: accessibility,
            target,
            profile,
            events: 0,
            group: unsafe { clock_gettime_nsec_np(libc::CLOCK_UPTIME_RAW) } as i64,
        })
    }

    fn prepare(&mut self) -> Result<(), String> {
        self.activation = Some(Activation::new(self.api, self.target)?);
        self.activation
            .as_mut()
            .unwrap()
            .prepare(self.target, self.profile.prepare_active)?;
        self.monitor.check()
    }

    fn post(&mut self, event: &CGEvent, route: Route) {
        if route == Route::SkyLight {
            event.set_integer_value_field(40, i64::from(self.target.pid));
        }
        unsafe {
            CGEventSetTimestamp(
                event.as_ptr(),
                clock_gettime_nsec_np(libc::CLOCK_UPTIME_RAW),
            );
        }
        match route {
            Route::Quartz => event.post_to_pid(self.target.pid),
            Route::SkyLight => unsafe { (self.api.post)(self.target.pid, event.as_ptr()) },
        }
        self.events += 1;
    }

    fn mouse(
        &self,
        kind: CGEventType,
        point: (i32, i32),
        count: i64,
        phase: i64,
    ) -> Result<CGEvent, String> {
        self.mouse_on_route(kind, point, count, phase, self.profile.pointer)
    }

    fn mouse_on_route(
        &self,
        kind: CGEventType,
        point: (i32, i32),
        count: i64,
        phase: i64,
        route: Route,
    ) -> Result<CGEvent, String> {
        let event = mac_mouse_event(
            self.target,
            kind,
            CGMouseButton::Left,
            point.0,
            point.1,
            count,
        )?;
        event.set_flags(CGEventFlags::CGEventFlagNull);
        if route == Route::SkyLight {
            for (field, value) in [
                (0, phase),
                (7, 3),
                (40, i64::from(self.target.pid)),
                (58, self.group),
            ] {
                event.set_integer_value_field(field, value);
            }
        }
        Ok(event)
    }

    fn pair(&mut self, down: &CGEvent, up: &CGEvent, route: Route) -> Result<(), String> {
        self.monitor.check()?;
        self.post(down, route);
        thread::sleep(Duration::from_millis(12));
        // Once down has been posted, always release even if monitoring detects an interruption.
        self.post(up, route);
        self.monitor.check()
    }

    fn click(&mut self, point: (i32, i32), count: i64) -> Result<(), String> {
        let moved = self.mouse(CGEventType::MouseMoved, point, 0, 2)?;
        let pairs = (1..=count)
            .map(|n| {
                Ok((
                    self.mouse(CGEventType::LeftMouseDown, point, n, 3)?,
                    self.mouse(CGEventType::LeftMouseUp, point, n, 3)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.monitor.check()?;
        self.post(&moved, self.profile.pointer);
        thread::sleep(Duration::from_millis(15));
        if self.profile.primer {
            // Negative WINDOW-local coordinates, never an unscoped desktop click.
            let outside = (self.target.x - 1, self.target.y - 1);
            let down = self.mouse(CGEventType::LeftMouseDown, outside, 1, 1)?;
            let up = self.mouse(CGEventType::LeftMouseUp, outside, 1, 2)?;
            self.pair(&down, &up, self.profile.pointer)?;
            thread::sleep(Duration::from_millis(100));
        }
        for (i, (down, up)) in pairs.iter().enumerate() {
            if i > 0 {
                thread::sleep(Duration::from_millis(80));
            }
            mac_validate_mouse_layout(self.target, self.target, point)?;
            self.pair(down, up, self.profile.pointer)?;
        }
        Ok(())
    }

    fn keyboard(&mut self, action: &ComputerAction) -> Result<Value, String> {
        enable_accessibility(&mac_ax_relations::application(self.target.pid)?);
        let prepared = mac_prepare_keyboard(self.target, DeliveryPolicy::StrictBackground)
            .map_err(|receipt| {
                receipt["error"]
                    .as_str()
                    .unwrap_or("Keyboard target unresolved")
                    .to_string()
            })?;
        self.target = prepared.target;
        let source = mac_event_source()?;
        let pairs: Vec<(CGEvent, CGEvent)> = if action.action == "type" {
            action
                .text
                .as_deref()
                .ok_or("text is required")?
                .chars()
                .map(|character| {
                    let make = |down| {
                        let event = CGEvent::new_keyboard_event(source.clone(), 0, down)
                            .map_err(|_| "Cannot create text event")?;
                        event.set_flags(CGEventFlags::CGEventFlagNull);
                        event.set_string(&character.to_string());
                        Ok::<_, String>(event)
                    };
                    Ok((make(true)?, make(false)?))
                })
                .collect::<Result<_, String>>()?
        } else {
            let (code, flags) = mac_key_combination(action.keys.as_deref().unwrap_or_default())?;
            let make = |down| {
                let event = CGEvent::new_keyboard_event(source.clone(), code, down)
                    .map_err(|_| "Cannot create key event")?;
                event.set_flags(flags);
                Ok::<_, String>(event)
            };
            vec![(make(true)?, make(false)?)]
        };
        for (down, up) in pairs {
            mac_verify_keyboard_focus(self.target, false)?;
            for event in [&down, &up] {
                event.set_integer_value_field(
                    CG_EVENT_TARGET_WINDOW,
                    i64::from(self.target.window_id),
                );
                event.set_integer_value_field(
                    CG_EVENT_RECEIVING_WINDOW,
                    i64::from(self.target.window_id),
                );
            }
            self.pair(&down, &up, self.profile.keyboard)?;
        }
        Ok(prepared.receipt)
    }

    fn cleanup(&mut self) -> Result<(), String> {
        self.activation
            .as_mut()
            .map(Activation::finish)
            .unwrap_or(Ok(()))
    }

    fn finish(&mut self) -> Result<(), String> {
        let cleanup = self.cleanup();
        let isolation = self.monitor.finish();
        cleanup.and(isolation)
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn run(action: &ComputerAction, target: WindowTarget, profile: &Profile) -> Value {
    // Serializes synthetic active-state lifetimes across sessions and windows.
    static INPUT: Mutex<()> = Mutex::new(());
    let _input = INPUT.lock().unwrap_or_else(|p| p.into_inner());
    let mut session = match Session::start(target, profile) {
        Ok(s) => s,
        Err(reason) => {
            let mut result = rejected(action, reason);
            result["error_code"] = json!("background_preparation_failed");
            result["focus_isolation"] = json!("unavailable");
            result["summary"] =
                json!("Background prerequisites could not be verified; no input was sent");
            return result;
        }
    };
    let before = mac_capture_window_group(target);
    let mut semantic = false;
    let mut keyboard_receipt = Value::Null;
    let dispatched = (|| -> Result<(), String> {
        let point = if matches!(
            action.action.as_str(),
            "click" | "double_click" | "move" | "drag" | "scroll"
        ) {
            let point = background_point(action, target)?;
            let resolved = mac_background_event_target(target, point)?;
            if resolved.target != target || !resolved.on_screen {
                return Err(
                    "Auxiliary/off-screen pointer target has not passed background validation"
                        .into(),
                );
            }
            mac_validate_mouse_layout(target, target, point)?;
            Some(point)
        } else {
            None
        };
        if action.action == "click" && profile.semantic_click {
            session.monitor.check()?;
            match mac_ax_press(target, point.unwrap().0, point.unwrap().1) {
                MacAxPressOutcome::Performed { .. } => {
                    semantic = true;
                    session.events += 1;
                    return Ok(());
                }
                MacAxPressOutcome::Uncertain { reason } => {
                    session.events += 1;
                    return Err(reason);
                }
                MacAxPressOutcome::Unsupported { .. } => {}
            }
        }
        session.prepare()?;
        if let Some(point) = point {
            mac_validate_mouse_layout(target, target, point)?;
        }
        match action.action.as_str() {
            "click" | "double_click" => session.click(
                point.unwrap(),
                if action.action == "double_click" {
                    2
                } else {
                    1
                },
            )?,
            "move" => {
                let event = session.mouse(CGEventType::MouseMoved, point.unwrap(), 0, 2)?;
                session.monitor.check()?;
                session.post(&event, profile.pointer);
            }
            "scroll" => {
                let (dx, dy) = scroll_delta(action)?;
                let route = profile.scroll.unwrap_or(profile.pointer);
                let moved =
                    session.mouse_on_route(CGEventType::MouseMoved, point.unwrap(), 0, 2, route)?;
                session.monitor.check()?;
                session.post(&moved, route);
                thread::sleep(Duration::from_millis(30));
                for (dx, dy) in scroll_steps(dx, dy) {
                    let event = mac_scroll_event(Some(target), point.unwrap(), dx, dy)?;
                    session.monitor.check()?;
                    session.post(&event, route);
                    thread::sleep(Duration::from_millis(16));
                }
            }
            "type" | "keypress" => {
                keyboard_receipt = session.keyboard(action)?;
            }
            _ => return Err("This gesture has no validated background implementation".into()),
        }
        Ok(())
    })();
    thread::sleep(Duration::from_millis(180));
    let cleanup = session.cleanup();
    let after = mac_capture_window_group(target);
    // Keep observing through screenshot collection and delayed application work.
    thread::sleep(Duration::from_millis(500));
    let cleanup_succeeded = cleanup.is_ok();
    let isolation = session.finish().and(cleanup);
    let point = action
        .x
        .zip(action.y)
        .map(|(x, y)| (target.x + x, target.y + y));
    let change = point.and_then(|point| {
        before
            .as_ref()
            .ok()
            .zip(after.as_ref().ok())
            .and_then(|(a, b)| screenshot_visual_change(a, b, point))
    });
    let error = dispatched.as_ref().err().or(isolation.as_ref().err());
    let mut result = json!({"ok": error.is_none(), "action": action.action, "mode": "background_app",
        "summary": if error.is_none() { "Background input dispatched; inspect the target to confirm its effect" } else { "Background input stopped; inspect the receipt before continuing" },
        "action_dispatched": session.events > 0, "dispatch_succeeded": dispatched.is_ok(), "native_events_sent": session.events,
        "effect_verified": false, "visual_change_detected": change.map(|v| v.detected),
        "focus_isolation": if isolation.is_ok() { "preserved" } else { "violated" },
        "isolation_verification": "sampled_front_process_cursor_all_display_spaces_window_order_and_foreground_ax_focus",
        "isolation_poll_sleep_ms": 2, "isolation_full_check_every_samples": 5,
        "isolation_samples": session.monitor.evidence,
        "cleanup_succeeded": cleanup_succeeded,
        "input_method": if semantic { "accessibility_action" } else if matches!(action.action.as_str(), "type" | "keypress") { match profile.keyboard { Route::Quartz => "quartz_keyboard", Route::SkyLight => "skylight_keyboard" } } else { match if action.action == "scroll" { profile.scroll.unwrap_or(profile.pointer) } else { profile.pointer } { Route::Quartz => "quartz_window", Route::SkyLight => "skylight_window" } },
        "background_target": profile.identity,
        "retry_safe": session.events == 0 && isolation.is_ok(), "requires_observation": true,
        "coordinate_space": "window"});
    if !keyboard_receipt.is_null() {
        result["keyboard"] = keyboard_receipt;
    }
    if let Some(reason) = error {
        result["error"] = json!(reason);
        result["error_code"] = json!(if isolation.is_err() {
            "background_isolation_interrupted"
        } else {
            "background_dispatch_interrupted"
        });
    }
    finish_background_click(action, result, after)
}
