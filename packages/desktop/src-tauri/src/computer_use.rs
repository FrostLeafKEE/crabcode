use base64::{engine::general_purpose::STANDARD, Engine as _};
#[cfg(not(target_os = "macos"))]
use enigo::Axis;
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Cursor;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use xcap::image::{imageops::FilterType, DynamicImage, ImageFormat, RgbaImage};
use xcap::{Monitor, Window};

#[cfg(target_os = "macos")]
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField, KeyCode,
    ScrollEventUnit,
};
#[cfg(target_os = "macos")]
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
#[cfg(target_os = "macos")]
use core_graphics::geometry::CGPoint;
#[cfg(target_os = "macos")]
use foreign_types::ForeignType;

const MAX_SCREENSHOT_BYTES: usize = 20 * 1024 * 1024;
const MAX_SCROLL_DELTA: i32 = 10_000;
#[cfg(target_os = "macos")]
const SCROLL_STEP_PIXELS: i32 = 50;
const SCROLL_SETTLE_MS: u64 = 180;

#[derive(Debug, Clone, Serialize)]
pub struct DisplayInfo {
    id: String,
    name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    primary: bool,
}

#[derive(Debug, Serialize)]
pub struct ComputerUseCapabilities {
    gui_available: bool,
    input_available: bool,
    platform: &'static str,
    displays: Vec<DisplayInfo>,
    supported_modes: Vec<&'static str>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExecuteRequest {
    #[serde(default)]
    mode: ComputerUseMode,
    action: ComputerAction,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ComputerUseMode {
    BackgroundApp,
    ForegroundDesktop,
}

impl Default for ComputerUseMode {
    fn default() -> Self {
        Self::BackgroundApp
    }
}

#[derive(Debug, Deserialize)]
pub struct ComputerAction {
    action: String,
    x: Option<i32>,
    y: Option<i32>,
    to_x: Option<i32>,
    to_y: Option<i32>,
    button: Option<String>,
    delta_x: Option<i32>,
    delta_y: Option<i32>,
    text: Option<String>,
    keys: Option<Vec<String>>,
    display_id: Option<String>,
    window_id: Option<String>,
    duration_ms: Option<u64>,
    include_screenshot: Option<bool>,
}

fn display_info(monitor: &Monitor) -> Result<DisplayInfo, String> {
    Ok(DisplayInfo {
        id: monitor.id().map_err(|error| error.to_string())?.to_string(),
        name: monitor
            .friendly_name()
            .or_else(|_| monitor.name())
            .map_err(|error| error.to_string())?,
        x: monitor.x().map_err(|error| error.to_string())?,
        y: monitor.y().map_err(|error| error.to_string())?,
        width: monitor.width().map_err(|error| error.to_string())?,
        height: monitor.height().map_err(|error| error.to_string())?,
        primary: monitor.is_primary().map_err(|error| error.to_string())?,
    })
}

fn monitors() -> Result<Vec<(Monitor, DisplayInfo)>, String> {
    Monitor::all()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|monitor| display_info(&monitor).map(|info| (monitor, info)))
        .collect()
}

fn capability_status(
    capture_error: Option<String>,
    input_error: Option<String>,
) -> (bool, bool, Option<String>) {
    let gui_available = capture_error.is_none();
    let input_available = input_error.is_none();
    let reason = capture_error.or(input_error);
    (gui_available, input_available, reason)
}

fn detect_capabilities() -> ComputerUseCapabilities {
    let found = match monitors() {
        Ok(found) if !found.is_empty() => found,
        Ok(_) => {
            return ComputerUseCapabilities {
                gui_available: false,
                input_available: false,
                platform: std::env::consts::OS,
                displays: Vec::new(),
                supported_modes: supported_modes(),
                reason: Some("No graphical displays were detected".to_string()),
            }
        }
        Err(error) => {
            return ComputerUseCapabilities {
                gui_available: false,
                input_available: false,
                platform: std::env::consts::OS,
                displays: Vec::new(),
                supported_modes: supported_modes(),
                reason: Some(error),
            }
        }
    };

    let displays = found
        .iter()
        .map(|(_, info)| info.clone())
        .collect::<Vec<_>>();
    let capture_target = found
        .iter()
        .find(|(_, info)| info.primary)
        .or_else(|| found.first());
    let capture_error = capture_target
        .and_then(|(monitor, _)| monitor.capture_image().err())
        .map(|error| format!("Screen capture is unavailable: {error}"));
    let input_error = Enigo::new(&Settings::default())
        .err()
        .map(|error| format!("Desktop input permission is unavailable: {error}"));
    // A missing input permission must not hide ComputerUse from the model. The
    // agent can still observe the desktop and use non-input actions such as
    // open_app. Only a missing/capture-inaccessible GUI disables the tool.
    let (gui_available, input_available, reason) = capability_status(capture_error, input_error);
    ComputerUseCapabilities {
        gui_available,
        input_available,
        platform: std::env::consts::OS,
        displays,
        supported_modes: supported_modes(),
        reason,
    }
}

#[tauri::command]
pub async fn computer_use_capabilities() -> ComputerUseCapabilities {
    tauri::async_runtime::spawn_blocking(detect_capabilities)
        .await
        .unwrap_or_else(|error| ComputerUseCapabilities {
            gui_available: false,
            input_available: false,
            platform: std::env::consts::OS,
            displays: Vec::new(),
            supported_modes: supported_modes(),
            reason: Some(format!("Computer Use capability detection failed: {error}")),
        })
}

fn supported_modes() -> Vec<&'static str> {
    #[cfg(target_os = "macos")]
    {
        vec!["background_app", "foreground_desktop"]
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec!["foreground_desktop"]
    }
}

#[tauri::command]
pub async fn computer_use_open_input_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return tauri::async_runtime::spawn_blocking(|| {
            Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .status()
                .map_err(|error| format!("Unable to open Accessibility settings: {error}"))
                .and_then(|status| {
                    if status.success() {
                        Ok(())
                    } else {
                        Err(format!(
                            "Unable to open Accessibility settings; process exited with {status}"
                        ))
                    }
                })
        })
        .await
        .map_err(|error| format!("Unable to open Accessibility settings: {error}"))?;
    }

    #[cfg(not(target_os = "macos"))]
    Err("Opening Accessibility settings is only supported on macOS".to_string())
}

fn required<T: Copy>(value: Option<T>, name: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("{name} is required"))
}

fn mouse_button(name: Option<&str>) -> Result<Button, String> {
    match name.unwrap_or("left").to_ascii_lowercase().as_str() {
        "left" => Ok(Button::Left),
        "middle" => Ok(Button::Middle),
        "right" => Ok(Button::Right),
        value => Err(format!("Unknown mouse button: {value}")),
    }
}

fn key_from_name(name: &str) -> Result<Key, String> {
    let upper = name.trim().to_ascii_uppercase();
    Ok(match upper.as_str() {
        "ALT" | "OPTION" => Key::Alt,
        "BACKSPACE" => Key::Backspace,
        "CAPSLOCK" | "CAPS_LOCK" => Key::CapsLock,
        "CMD" | "COMMAND" | "META" | "WIN" | "WINDOWS" => Key::Meta,
        "CTRL" | "CONTROL" => Key::Control,
        "DELETE" | "DEL" => Key::Delete,
        "DOWN" | "ARROWDOWN" => Key::DownArrow,
        "END" => Key::End,
        "ENTER" | "RETURN" => Key::Return,
        "ESC" | "ESCAPE" => Key::Escape,
        "HOME" => Key::Home,
        "LEFT" | "ARROWLEFT" => Key::LeftArrow,
        "PAGEDOWN" | "PAGE_DOWN" => Key::PageDown,
        "PAGEUP" | "PAGE_UP" => Key::PageUp,
        "RIGHT" | "ARROWRIGHT" => Key::RightArrow,
        "SHIFT" => Key::Shift,
        "SPACE" => Key::Space,
        "TAB" => Key::Tab,
        "UP" | "ARROWUP" => Key::UpArrow,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        _ => {
            let mut chars = name.chars();
            let character = chars
                .next()
                .ok_or_else(|| "Key cannot be empty".to_string())?;
            if chars.next().is_some() {
                return Err(format!("Unknown key: {name}"));
            }
            Key::Unicode(character)
        }
    })
}

fn press_keys(enigo: &mut Enigo, names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Err("keys cannot be empty".to_string());
    }
    let keys = names
        .iter()
        .map(|name| key_from_name(name))
        .collect::<Result<Vec<_>, _>>()?;
    let mut pressed = Vec::with_capacity(keys.len());
    for key in &keys {
        if let Err(error) = enigo.key(*key, Direction::Press) {
            for pressed_key in pressed.iter().rev() {
                let _ = enigo.key(*pressed_key, Direction::Release);
            }
            return Err(error.to_string());
        }
        pressed.push(*key);
    }
    let mut release_error = None;
    for key in pressed.iter().rev() {
        if let Err(error) = enigo.key(*key, Direction::Release) {
            release_error.get_or_insert_with(|| error.to_string());
        }
    }
    if let Some(error) = release_error {
        return Err(error);
    }
    Ok(())
}

fn activate_app(name: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = Command::new("open").args(["-a", name]).status();

    #[cfg(target_os = "windows")]
    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Start-Process -FilePath $args[0]",
            name,
        ])
        .creation_flags(0x08000000)
        .status();

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let status = Command::new(name).status();

    match status {
        Ok(value) if value.success() => Ok(()),
        Ok(value) => Err(format!("Unable to open app; process exited with {value}")),
        Err(error) => Err(format!("Unable to open app: {error}")),
    }
}

fn focus_app(name: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = Command::new("osascript")
        .args([
            "-e",
            "on run argv\nset appName to item 1 of argv\ntell application \"System Events\" to set frontmost of process appName to true\nend run",
            name,
        ])
        .status();

    #[cfg(target_os = "windows")]
    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "$ws=New-Object -ComObject WScript.Shell; if(-not $ws.AppActivate($args[0])){exit 1}",
            name,
        ])
        .creation_flags(0x08000000)
        .status();

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Err("Window focus is currently supported on macOS and Windows".to_string());

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    match status {
        Ok(value) if value.success() => Ok(()),
        Ok(value) => Err(format!("Unable to focus app; process exited with {value}")),
        Err(error) => Err(format!("Unable to focus app: {error}")),
    }
}

fn window_list() -> Result<Vec<Value>, String> {
    Window::all()
        .map_err(|error| error.to_string())?
        .iter()
        .map(|window| {
            Ok(json!({
                "id": window.id().map_err(|error| error.to_string())?.to_string(),
                "pid": window.pid().map_err(|error| error.to_string())?,
                "app_name": window.app_name().map_err(|error| error.to_string())?,
                "title": window.title().map_err(|error| error.to_string())?,
                "x": window.x().map_err(|error| error.to_string())?,
                "y": window.y().map_err(|error| error.to_string())?,
                "width": window.width().map_err(|error| error.to_string())?,
                "height": window.height().map_err(|error| error.to_string())?,
                "focused": window.is_focused().map_err(|error| error.to_string())?,
            }))
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct WindowTarget {
    window_id: u32,
    pid: i32,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn window_target(window_id: &str) -> Result<WindowTarget, String> {
    let windows = Window::all().map_err(|error| error.to_string())?;
    let window = windows
        .iter()
        .find(|window| {
            window
                .id()
                .map(|value| value.to_string() == window_id)
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("Window not found: {window_id}"))?;
    Ok(WindowTarget {
        window_id: window.id().map_err(|error| error.to_string())?,
        pid: window.pid().map_err(|error| error.to_string())? as i32,
        x: window.x().map_err(|error| error.to_string())?,
        y: window.y().map_err(|error| error.to_string())?,
        width: window.width().map_err(|error| error.to_string())?,
        height: window.height().map_err(|error| error.to_string())?,
    })
}

fn background_target(action: &ComputerAction) -> Result<WindowTarget, String> {
    let window_id = action.window_id.as_deref().ok_or_else(|| {
        "background_app mode requires window_id; call list_windows first".to_string()
    })?;
    window_target(window_id)
}

fn background_point(action: &ComputerAction, target: WindowTarget) -> Result<(i32, i32), String> {
    let x = required(action.x, "x")?;
    let y = required(action.y, "y")?;
    let right = target.x.saturating_add(target.width as i32);
    let bottom = target.y.saturating_add(target.height as i32);
    if x < target.x || x >= right || y < target.y || y >= bottom {
        return Err(format!(
            "Point ({x}, {y}) is outside target window bounds ({}, {}) {}x{}",
            target.x, target.y, target.width, target.height
        ));
    }
    Ok((x, y))
}

#[cfg(target_os = "macos")]
fn mac_event_source() -> Result<CGEventSource, String> {
    CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| "Unable to create a private macOS input source".to_string())
}

#[cfg(target_os = "macos")]
fn mac_require_input_permission() -> Result<(), String> {
    let settings = Settings {
        open_prompt_to_get_permissions: false,
        ..Settings::default()
    };
    Enigo::new(&settings)
        .map(|_| ())
        .map_err(|error| format!("macOS Accessibility permission is required: {error}"))
}

#[cfg(target_os = "macos")]
fn mac_mouse_button(name: Option<&str>) -> Result<CGMouseButton, String> {
    match name.unwrap_or("left").to_ascii_lowercase().as_str() {
        "left" => Ok(CGMouseButton::Left),
        "middle" => Ok(CGMouseButton::Center),
        "right" => Ok(CGMouseButton::Right),
        value => Err(format!("Unknown mouse button: {value}")),
    }
}

#[cfg(target_os = "macos")]
fn mac_mouse_event_types(button: CGMouseButton) -> (CGEventType, CGEventType, CGEventType) {
    match button {
        CGMouseButton::Left => (
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::LeftMouseDragged,
        ),
        CGMouseButton::Right => (
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGEventType::RightMouseDragged,
        ),
        CGMouseButton::Center => (
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGEventType::OtherMouseDragged,
        ),
    }
}

#[cfg(target_os = "macos")]
const CG_EVENT_TARGET_WINDOW: u32 = 51;
#[cfg(target_os = "macos")]
const CG_EVENT_RECEIVING_WINDOW: u32 = 52;

#[cfg(target_os = "macos")]
fn mac_target_pointer_event(event: &CGEvent, target: WindowTarget) -> Result<(), String> {
    type SetWindowLocation = unsafe extern "C" fn(core_graphics::sys::CGEventRef, CGPoint);
    static SET_WINDOW_LOCATION: std::sync::OnceLock<Option<SetWindowLocation>> =
        std::sync::OnceLock::new();
    let set_window_location = SET_WINDOW_LOCATION.get_or_init(|| {
        // This private CoreGraphics symbol preserves the local point when
        // posting to a non-key window. Resolve at runtime so an OS without it
        // can still start the app and use observation/foreground control.
        let symbol =
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"CGEventSetWindowLocation".as_ptr()) };
        if symbol.is_null() {
            None
        } else {
            // SAFETY: the symbol has the CGEventRef/CGPoint C ABI above, and
            // CoreGraphics stays loaded for the entire process lifetime.
            Some(unsafe { std::mem::transmute::<*mut libc::c_void, SetWindowLocation>(symbol) })
        }
    });
    let set_window_location = set_window_location.ok_or_else(|| {
        "Window-targeted background input is unavailable on this macOS version; choose foreground_desktop explicitly"
            .to_string()
    })?;
    // A PID can own multiple overlapping windows. Preserve the selected
    // CGWindowID for AppKit's event routing, including synthetic MouseMoved.
    // The public mouse-only fields 91/92 are ignored on wheel events; slots
    // 51/52 carry the receiving window number for both event types.
    event.set_integer_value_field(CG_EVENT_TARGET_WINDOW, i64::from(target.window_id));
    event.set_integer_value_field(CG_EVENT_RECEIVING_WINDOW, i64::from(target.window_id));
    event.set_integer_value_field(
        EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER,
        i64::from(target.window_id),
    );
    event.set_integer_value_field(
        EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER_THAT_CAN_HANDLE_THIS_EVENT,
        i64::from(target.window_id),
    );
    let global = event.location();
    let local = CGPoint::new(
        global.x - f64::from(target.x),
        global.y - f64::from(target.y),
    );
    // SAFETY: a live, owned CGEvent is passed with a window-local point.
    unsafe { set_window_location(event.as_ptr(), local) };
    if event.get_integer_value_field(CG_EVENT_TARGET_WINDOW) != i64::from(target.window_id) {
        return Err("macOS did not preserve the background event's target window".to_string());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn mac_post_mouse(
    target: WindowTarget,
    event_type: CGEventType,
    button: CGMouseButton,
    x: i32,
    y: i32,
    click_count: i64,
) -> Result<(), String> {
    let event = CGEvent::new_mouse_event(
        mac_event_source()?,
        event_type,
        CGPoint::new(x as f64, y as f64),
        button,
    )
    .map_err(|_| "Unable to create a macOS mouse event".to_string())?;
    event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_count);
    mac_target_pointer_event(&event, target)?;
    event.post_to_pid(target.pid);
    Ok(())
}

#[cfg(target_os = "macos")]
fn mac_click(
    target: WindowTarget,
    x: i32,
    y: i32,
    button_name: Option<&str>,
    click_count: i64,
) -> Result<(), String> {
    let button = mac_mouse_button(button_name)?;
    let (down, up, _) = mac_mouse_event_types(button);
    for count in 1..=click_count {
        mac_post_mouse(target, down, button, x, y, count)?;
        mac_post_mouse(target, up, button, x, y, count)?;
        if count < click_count {
            thread::sleep(Duration::from_millis(80));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn mac_drag(
    target: WindowTarget,
    start: (i32, i32),
    end: (i32, i32),
    button_name: Option<&str>,
    duration_ms: u64,
) -> Result<(), String> {
    let button = mac_mouse_button(button_name)?;
    let (down, up, dragged) = mac_mouse_event_types(button);
    mac_post_mouse(target, down, button, start.0, start.1, 1)?;
    let steps = 20i32;
    for step in 1..=steps {
        let x = start.0 + (end.0 - start.0) * step / steps;
        let y = start.1 + (end.1 - start.1) * step / steps;
        mac_post_mouse(target, dragged, button, x, y, 1)?;
        thread::sleep(Duration::from_millis(duration_ms / steps as u64));
    }
    mac_post_mouse(target, up, button, end.0, end.1, 1)
}

fn scroll_delta(action: &ComputerAction) -> Result<(i32, i32), String> {
    if action.x.is_some() != action.y.is_some() {
        return Err("x and y must be provided together for scroll".to_string());
    }
    let dx = action.delta_x.unwrap_or(0);
    let dy = action.delta_y.unwrap_or(0);
    if dx == 0 && dy == 0 {
        return Err("scroll requires a non-zero delta_x or delta_y".to_string());
    }
    if !(-MAX_SCROLL_DELTA..=MAX_SCROLL_DELTA).contains(&dx)
        || !(-MAX_SCROLL_DELTA..=MAX_SCROLL_DELTA).contains(&dy)
    {
        return Err(format!(
            "scroll deltas must be between -{MAX_SCROLL_DELTA} and {MAX_SCROLL_DELTA}"
        ));
    }
    Ok((dx, dy))
}

#[cfg(target_os = "macos")]
fn scroll_steps(delta_x: i32, delta_y: i32) -> Vec<(i32, i32)> {
    let count =
        ((delta_x.abs().max(delta_y.abs()) + SCROLL_STEP_PIXELS - 1) / SCROLL_STEP_PIXELS).max(1);
    // Distribute rounding remainders so diagonal scrolling preserves both totals.
    (0..count)
        .map(|i| {
            (
                delta_x * (i + 1) / count - delta_x * i / count,
                delta_y * (i + 1) / count - delta_y * i / count,
            )
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn mac_scroll_event(
    target: Option<WindowTarget>,
    point: (i32, i32),
    delta_x: i32,
    delta_y: i32,
) -> Result<CGEvent, String> {
    let event = CGEvent::new_scroll_event(
        mac_event_source()?,
        ScrollEventUnit::PIXEL,
        2,
        -delta_y,
        -delta_x,
        0,
    )
    .map_err(|_| "Unable to create a macOS scroll event".to_string())?;
    // Public deltas follow viewport movement: positive down/right. Quartz's
    // wheel signs are the opposite. Both macOS modes use this same conversion.
    event.set_location(CGPoint::new(point.0 as f64, point.1 as f64));
    if let Some(target) = target {
        mac_target_pointer_event(&event, target)?;
    }
    Ok(event)
}

#[cfg(target_os = "macos")]
fn mac_scroll(
    target: Option<WindowTarget>,
    point: (i32, i32),
    delta_x: i32,
    delta_y: i32,
) -> Result<(), String> {
    if let Some(target) = target {
        mac_post_mouse(
            target,
            CGEventType::MouseMoved,
            CGMouseButton::Left,
            point.0,
            point.1,
            0,
        )?;
    }
    // Let the application update its hover/hit-test state before the wheel input.
    thread::sleep(Duration::from_millis(30));
    for (dx, dy) in scroll_steps(delta_x, delta_y) {
        let event = mac_scroll_event(target, point, dx, dy)?;
        if let Some(target) = target {
            event.post_to_pid(target.pid);
        } else {
            event.post(CGEventTapLocation::HID);
        }
        thread::sleep(Duration::from_millis(16));
    }
    Ok(())
}

fn add_scroll_receipt(result: &mut Value, action: &ComputerAction) {
    if action.action != "scroll" {
        return;
    }
    result["effect_verified"] = json!(false);
    result["scroll"] = json!({
        "unit": if cfg!(target_os = "macos") { "pixels" } else { "wheel_steps" },
        "delta_x": action.delta_x.unwrap_or(0),
        "delta_y": action.delta_y.unwrap_or(0),
    });
    result["verification_hint"] = json!(
        "Input was sent; target scrolling is not confirmed. Compare the target scroll area before and after. \
         An unchanged image does not prove a history boundary. Background support varies by app; \
         do not automatically switch to foreground control."
    );
}

#[cfg(target_os = "macos")]
fn mac_keycode(name: &str) -> Option<u16> {
    Some(match name.trim().to_ascii_uppercase().as_str() {
        "A" => KeyCode::ANSI_A,
        "B" => KeyCode::ANSI_B,
        "C" => KeyCode::ANSI_C,
        "D" => KeyCode::ANSI_D,
        "E" => KeyCode::ANSI_E,
        "F" => KeyCode::ANSI_F,
        "G" => KeyCode::ANSI_G,
        "H" => KeyCode::ANSI_H,
        "I" => KeyCode::ANSI_I,
        "J" => KeyCode::ANSI_J,
        "K" => KeyCode::ANSI_K,
        "L" => KeyCode::ANSI_L,
        "M" => KeyCode::ANSI_M,
        "N" => KeyCode::ANSI_N,
        "O" => KeyCode::ANSI_O,
        "P" => KeyCode::ANSI_P,
        "Q" => KeyCode::ANSI_Q,
        "R" => KeyCode::ANSI_R,
        "S" => KeyCode::ANSI_S,
        "T" => KeyCode::ANSI_T,
        "U" => KeyCode::ANSI_U,
        "V" => KeyCode::ANSI_V,
        "W" => KeyCode::ANSI_W,
        "X" => KeyCode::ANSI_X,
        "Y" => KeyCode::ANSI_Y,
        "Z" => KeyCode::ANSI_Z,
        "0" => KeyCode::ANSI_0,
        "1" => KeyCode::ANSI_1,
        "2" => KeyCode::ANSI_2,
        "3" => KeyCode::ANSI_3,
        "4" => KeyCode::ANSI_4,
        "5" => KeyCode::ANSI_5,
        "6" => KeyCode::ANSI_6,
        "7" => KeyCode::ANSI_7,
        "8" => KeyCode::ANSI_8,
        "9" => KeyCode::ANSI_9,
        "ENTER" | "RETURN" => KeyCode::RETURN,
        "TAB" => KeyCode::TAB,
        "SPACE" => KeyCode::SPACE,
        "BACKSPACE" => KeyCode::DELETE,
        "DELETE" | "DEL" => KeyCode::FORWARD_DELETE,
        "ESC" | "ESCAPE" => KeyCode::ESCAPE,
        "LEFT" | "ARROWLEFT" => KeyCode::LEFT_ARROW,
        "RIGHT" | "ARROWRIGHT" => KeyCode::RIGHT_ARROW,
        "UP" | "ARROWUP" => KeyCode::UP_ARROW,
        "DOWN" | "ARROWDOWN" => KeyCode::DOWN_ARROW,
        "HOME" => KeyCode::HOME,
        "END" => KeyCode::END,
        "PAGEUP" | "PAGE_UP" => KeyCode::PAGE_UP,
        "PAGEDOWN" | "PAGE_DOWN" => KeyCode::PAGE_DOWN,
        "F1" => KeyCode::F1,
        "F10" => KeyCode::F10,
        "F11" => KeyCode::F11,
        "F12" => KeyCode::F12,
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
fn mac_press_keys(pid: i32, names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Err("keys cannot be empty".to_string());
    }
    let mut flags = CGEventFlags::CGEventFlagNull;
    let mut key_name: Option<&str> = None;
    for name in names {
        match name.trim().to_ascii_uppercase().as_str() {
            "CMD" | "COMMAND" | "META" => flags |= CGEventFlags::CGEventFlagCommand,
            "CTRL" | "CONTROL" => flags |= CGEventFlags::CGEventFlagControl,
            "ALT" | "OPTION" => flags |= CGEventFlags::CGEventFlagAlternate,
            "SHIFT" => flags |= CGEventFlags::CGEventFlagShift,
            _ if key_name.is_none() => key_name = Some(name),
            _ => return Err("background_app keypress supports one non-modifier key".to_string()),
        }
    }
    let name = key_name.ok_or_else(|| "keypress requires a non-modifier key".to_string())?;
    let keycode = mac_keycode(name).ok_or_else(|| format!("Unknown macOS key: {name}"))?;
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(mac_event_source()?, keycode, down)
            .map_err(|_| "Unable to create a macOS keyboard event".to_string())?;
        event.set_flags(flags);
        event.post_to_pid(pid);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn mac_type_text(pid: i32, text: &str) -> Result<(), String> {
    for character in text.chars() {
        let value = character.to_string();
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(mac_event_source()?, 0, down)
                .map_err(|_| "Unable to create a macOS text event".to_string())?;
            event.set_string(&value);
            event.post_to_pid(pid);
        }
    }
    Ok(())
}

fn screenshot(action: &ComputerAction) -> Result<Value, String> {
    let (image, x, y, target) = if let Some(window_id) = action.window_id.as_deref() {
        let windows = Window::all().map_err(|error| error.to_string())?;
        let window = windows
            .into_iter()
            .find(|window| {
                window
                    .id()
                    .map(|id| id.to_string() == window_id)
                    .unwrap_or(false)
            })
            .ok_or_else(|| format!("Window not found: {window_id}"))?;
        let x = window.x().map_err(|error| error.to_string())?;
        let y = window.y().map_err(|error| error.to_string())?;
        let width = window.width().map_err(|error| error.to_string())?;
        let height = window.height().map_err(|error| error.to_string())?;
        let image = normalize_image(
            window.capture_image().map_err(|error| error.to_string())?,
            width,
            height,
        );
        (image, x, y, format!("window:{window_id}"))
    } else {
        let found = monitors()?;
        let (monitor, info) = if let Some(display_id) = action.display_id.as_deref() {
            found
                .iter()
                .find(|(_, info)| info.id == display_id)
                .cloned()
                .ok_or_else(|| format!("Display not found: {display_id}"))?
        } else if let Some((x, y)) = action
            .to_x
            .zip(action.to_y)
            .or_else(|| action.x.zip(action.y))
        {
            let monitor = Monitor::from_point(x, y).map_err(|error| error.to_string())?;
            let info = display_info(&monitor)?;
            (monitor, info)
        } else {
            found
                .iter()
                .find(|(_, info)| info.primary)
                .or_else(|| found.first())
                .cloned()
                .ok_or_else(|| "No display is available".to_string())?
        };
        let image = normalize_image(
            monitor.capture_image().map_err(|error| error.to_string())?,
            info.width,
            info.height,
        );
        (image, info.x, info.y, format!("display:{}", info.id))
    };
    encode_screenshot(image, x, y, target)
}

fn normalize_image(image: RgbaImage, coordinate_width: u32, coordinate_height: u32) -> RgbaImage {
    if image.width() == coordinate_width && image.height() == coordinate_height {
        return image;
    }
    DynamicImage::ImageRgba8(image)
        .resize_exact(coordinate_width, coordinate_height, FilterType::Triangle)
        .to_rgba8()
}

fn encode_screenshot(image: RgbaImage, x: i32, y: i32, target: String) -> Result<Value, String> {
    let width = image.width();
    let height = image.height();
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    if bytes.get_ref().len() > MAX_SCREENSHOT_BYTES {
        return Err("Screenshot exceeds the 20MB transport limit".to_string());
    }
    let frame_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos().to_string())
        .unwrap_or_else(|_| "0".to_string());
    Ok(json!({
        "data": STANDARD.encode(bytes.into_inner()),
        "media_type": "image/png",
        "width": width,
        "height": height,
        "origin_x": x,
        "origin_y": y,
        "frame_id": frame_id,
        "target": target,
    }))
}

fn perform_action(action: &ComputerAction, enigo: &mut Enigo) -> Result<String, String> {
    match action.action.as_str() {
        "observe" | "list_displays" | "list_windows" => Ok(action.action.clone()),
        "move" => {
            enigo
                .move_mouse(
                    required(action.x, "x")?,
                    required(action.y, "y")?,
                    Coordinate::Abs,
                )
                .map_err(|error| error.to_string())?;
            Ok("Moved pointer".to_string())
        }
        "click" | "double_click" => {
            if let (Some(x), Some(y)) = (action.x, action.y) {
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|error| error.to_string())?;
            }
            let button = mouse_button(action.button.as_deref())?;
            enigo
                .button(button, Direction::Click)
                .map_err(|error| error.to_string())?;
            if action.action == "double_click" {
                thread::sleep(Duration::from_millis(80));
                enigo
                    .button(button, Direction::Click)
                    .map_err(|error| error.to_string())?;
            }
            Ok(if action.action == "double_click" {
                "Double-clicked"
            } else {
                "Clicked"
            }
            .to_string())
        }
        "drag" => {
            let start_x = required(action.x, "x")?;
            let start_y = required(action.y, "y")?;
            let end_x = required(action.to_x, "to_x")?;
            let end_y = required(action.to_y, "to_y")?;
            let button = mouse_button(action.button.as_deref())?;
            enigo
                .move_mouse(start_x, start_y, Coordinate::Abs)
                .map_err(|error| error.to_string())?;
            enigo
                .button(button, Direction::Press)
                .map_err(|error| error.to_string())?;
            let duration = action.duration_ms.unwrap_or(400).min(30_000);
            let steps = 20i32;
            let movement_result = (|| {
                for step in 1..=steps {
                    let x = start_x + (end_x - start_x) * step / steps;
                    let y = start_y + (end_y - start_y) * step / steps;
                    enigo
                        .move_mouse(x, y, Coordinate::Abs)
                        .map_err(|error| error.to_string())?;
                    thread::sleep(Duration::from_millis(duration / steps as u64));
                }
                Ok::<(), String>(())
            })();
            let release_result = enigo
                .button(button, Direction::Release)
                .map_err(|error| error.to_string());
            movement_result?;
            release_result?;
            Ok("Dragged pointer".to_string())
        }
        "scroll" => {
            let (delta_x, delta_y) = scroll_delta(action)?;
            if let (Some(x), Some(y)) = (action.x, action.y) {
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|error| error.to_string())?;
            }
            #[cfg(target_os = "macos")]
            {
                // A posted mouse move may not have updated the hardware
                // cursor yet. Keep the requested point instead of re-reading it.
                let point = match (action.x, action.y) {
                    (Some(x), Some(y)) => (x, y),
                    _ => enigo.location().map_err(|error| error.to_string())?,
                };
                mac_scroll(None, point, delta_x, delta_y)?;
            }
            #[cfg(not(target_os = "macos"))]
            {
                if delta_x != 0 {
                    enigo
                        .scroll(delta_x, Axis::Horizontal)
                        .map_err(|error| error.to_string())?;
                }
                if delta_y != 0 {
                    enigo
                        .scroll(delta_y, Axis::Vertical)
                        .map_err(|error| error.to_string())?;
                }
            }
            Ok("Scroll input sent; movement unverified".to_string())
        }
        "type" => {
            enigo
                .text(
                    action
                        .text
                        .as_deref()
                        .ok_or_else(|| "text is required".to_string())?,
                )
                .map_err(|error| error.to_string())?;
            Ok("Typed text".to_string())
        }
        "keypress" => {
            press_keys(enigo, action.keys.as_deref().unwrap_or_default())?;
            Ok("Pressed keys".to_string())
        }
        "open_app" => {
            let name = action
                .text
                .as_deref()
                .ok_or_else(|| "text is required".to_string())?;
            activate_app(name)?;
            Ok(format!("Opened {name}"))
        }
        "focus_window" => {
            let name = if let Some(name) = action.text.as_deref() {
                name.to_string()
            } else {
                let id = action
                    .window_id
                    .as_deref()
                    .ok_or_else(|| "window_id or text is required".to_string())?;
                Window::all()
                    .map_err(|error| error.to_string())?
                    .iter()
                    .find(|window| {
                        window
                            .id()
                            .map(|value| value.to_string() == id)
                            .unwrap_or(false)
                    })
                    .ok_or_else(|| format!("Window not found: {id}"))?
                    .app_name()
                    .map_err(|error| error.to_string())?
            };
            focus_app(&name)?;
            Ok(format!("Focused {name}"))
        }
        "wait" => {
            thread::sleep(Duration::from_millis(
                action.duration_ms.unwrap_or(500).min(30_000),
            ));
            Ok("Waited".to_string())
        }
        other => Err(format!("Unknown Computer Use action: {other}")),
    }
}

fn execute_foreground(request: ExecuteRequest) -> Result<Value, String> {
    let mut enigo = Enigo::new(&Settings::default()).map_err(|error| error.to_string())?;
    let summary = perform_action(&request.action, &mut enigo)?;
    let cursor = enigo
        .location()
        .map(|(x, y)| json!({ "x": x, "y": y }))
        .unwrap_or(Value::Null);

    let mut result = json!({
        "ok": true,
        "mode": "foreground_desktop",
        "action": request.action.action,
        "summary": summary,
        "cursor": cursor,
    });
    add_scroll_receipt(&mut result, &request.action);
    if request.action.action == "list_displays" {
        let displays = monitors()?
            .into_iter()
            .map(|(_, info)| info)
            .collect::<Vec<_>>();
        result["displays"] = serde_json::to_value(displays).map_err(|error| error.to_string())?;
    } else if request.action.action == "list_windows" {
        result["windows"] = Value::Array(window_list()?);
    }

    let capture = request.action.action == "observe"
        || request.action.include_screenshot.unwrap_or(!matches!(
            request.action.action.as_str(),
            "list_displays" | "list_windows" | "wait"
        ));
    if capture {
        if request.action.action == "scroll" {
            thread::sleep(Duration::from_millis(SCROLL_SETTLE_MS));
        }
        match screenshot(&request.action) {
            Ok(frame) => result["screenshot"] = frame,
            Err(error) if request.action.action == "observe" => return Err(error),
            Err(error) => result["screenshot_error"] = Value::String(error),
        }
    }
    Ok(result)
}

#[cfg(target_os = "macos")]
fn execute_background(request: ExecuteRequest) -> Result<Value, String> {
    let action = &request.action;
    if matches!(
        action.action.as_str(),
        "move" | "click" | "double_click" | "drag" | "scroll" | "type" | "keypress"
    ) {
        mac_require_input_permission()?;
    }
    let mut cursor = Value::Null;
    let summary = match action.action.as_str() {
        "observe" => {
            background_target(action)?;
            "Observed application window".to_string()
        }
        "list_windows" => "Listed application windows".to_string(),
        "list_displays" => {
            return Err(
                "background_app mode does not expose the full desktop or displays; use list_windows"
                    .to_string(),
            )
        }
        "move" => {
            let target = background_target(action)?;
            let (x, y) = background_point(action, target)?;
            mac_post_mouse(
                target,
                CGEventType::MouseMoved,
                CGMouseButton::Left,
                x,
                y,
                0,
            )?;
            cursor = json!({ "x": x, "y": y });
            "Moved application pointer".to_string()
        }
        "click" | "double_click" => {
            let target = background_target(action)?;
            let (x, y) = background_point(action, target)?;
            mac_click(
                target,
                x,
                y,
                action.button.as_deref(),
                if action.action == "double_click" { 2 } else { 1 },
            )?;
            cursor = json!({ "x": x, "y": y });
            if action.action == "double_click" {
                "Double-clicked application window"
            } else {
                "Clicked application window"
            }
            .to_string()
        }
        "drag" => {
            let target = background_target(action)?;
            let start = background_point(action, target)?;
            let end_action = ComputerAction {
                action: action.action.clone(),
                x: action.to_x,
                y: action.to_y,
                to_x: None,
                to_y: None,
                button: None,
                delta_x: None,
                delta_y: None,
                text: None,
                keys: None,
                display_id: None,
                window_id: action.window_id.clone(),
                duration_ms: None,
                include_screenshot: None,
            };
            let end = background_point(&end_action, target)?;
            mac_drag(
                target,
                start,
                end,
                action.button.as_deref(),
                action.duration_ms.unwrap_or(400).min(30_000),
            )?;
            cursor = json!({ "x": end.0, "y": end.1 });
            "Dragged in application window".to_string()
        }
        "scroll" => {
            let (delta_x, delta_y) = scroll_delta(action)?;
            let target = background_target(action)?;
            let point = background_point(action, target)?;
            mac_scroll(Some(target), point, delta_x, delta_y)?;
            cursor = json!({ "x": point.0, "y": point.1 });
            "Scroll input sent to application window; movement unverified".to_string()
        }
        "type" => {
            let target = background_target(action)?;
            mac_type_text(
                target.pid,
                action
                    .text
                    .as_deref()
                    .ok_or_else(|| "text is required".to_string())?,
            )?;
            "Typed into application window".to_string()
        }
        "keypress" => {
            let target = background_target(action)?;
            mac_press_keys(target.pid, action.keys.as_deref().unwrap_or_default())?;
            "Pressed keys in application window".to_string()
        }
        "open_app" => {
            let name = action
                .text
                .as_deref()
                .ok_or_else(|| "text is required".to_string())?;
            let status = Command::new("open")
                .args(["-g", "-a", name])
                .status()
                .map_err(|error| format!("Unable to open app in background: {error}"))?;
            if !status.success() {
                return Err(format!("Unable to open app in background; process exited with {status}"));
            }
            format!("Opened {name} in background")
        }
        "focus_window" => {
            return Err(
                "background_app mode cannot focus or raise a window; switch explicitly to foreground_desktop"
                    .to_string(),
            )
        }
        "wait" => {
            thread::sleep(Duration::from_millis(
                action.duration_ms.unwrap_or(500).min(30_000),
            ));
            "Waited".to_string()
        }
        other => return Err(format!("Unknown Computer Use action: {other}")),
    };

    let mut result = json!({
        "ok": true,
        "mode": "background_app",
        "action": action.action,
        "summary": summary,
        "cursor": cursor,
    });
    add_scroll_receipt(&mut result, action);
    if action.action == "list_windows" {
        result["windows"] = Value::Array(window_list()?);
    }

    let capture = action.action == "observe"
        || action.include_screenshot.unwrap_or(!matches!(
            action.action.as_str(),
            "list_windows" | "wait" | "open_app"
        ));
    if capture {
        if action.action == "scroll" {
            thread::sleep(Duration::from_millis(SCROLL_SETTLE_MS));
        }
        match screenshot(action) {
            Ok(frame) => result["screenshot"] = frame,
            Err(error) if action.action == "observe" => return Err(error),
            Err(error) => result["screenshot_error"] = Value::String(error),
        }
    }
    Ok(result)
}

#[cfg(not(target_os = "macos"))]
fn execute_background(_request: ExecuteRequest) -> Result<Value, String> {
    Err(
        "background_app mode is unavailable on this platform; switch explicitly to foreground_desktop"
            .to_string(),
    )
}

fn execute(request: ExecuteRequest) -> Result<Value, String> {
    match request.mode {
        ComputerUseMode::BackgroundApp => execute_background(request),
        ComputerUseMode::ForegroundDesktop => execute_foreground(request),
    }
}

#[tauri::command]
pub async fn computer_use_execute(request: ExecuteRequest) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || execute(request))
        .await
        .map_err(|error| format!("Computer Use worker failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_shortcut_keys() {
        assert_eq!(key_from_name("ctrl").unwrap(), Key::Control);
        assert_eq!(key_from_name("Enter").unwrap(), Key::Return);
        assert_eq!(key_from_name("x").unwrap(), Key::Unicode('x'));
        assert!(key_from_name("not-a-key").is_err());
    }

    #[test]
    fn rejects_unknown_mouse_button() {
        assert!(mouse_button(Some("sideways")).is_err());
    }

    #[test]
    fn scroll_rejects_noops_unpaired_coordinates_and_excessive_deltas() {
        for value in [
            json!({"action": "scroll"}),
            json!({"action": "scroll", "delta_y": 0}),
            json!({"action": "scroll", "x": 100, "delta_y": 800}),
            json!({"action": "scroll", "delta_y": i32::MIN}),
            json!({"action": "scroll", "delta_y": MAX_SCROLL_DELTA + 1}),
        ] {
            let action: ComputerAction = serde_json::from_value(value).unwrap();
            assert!(scroll_delta(&action).is_err());
        }
        let action: ComputerAction = serde_json::from_value(json!({
            "action": "scroll", "x": 1400, "y": 700, "delta_y": -800
        }))
        .unwrap();
        assert_eq!(scroll_delta(&action).unwrap(), (0, -800));
        let mut result = json!({"ok": true});
        add_scroll_receipt(&mut result, &action);
        assert_eq!(result["ok"], true);
        assert_eq!(result["effect_verified"], false);
        assert_eq!(result["scroll"]["delta_y"], -800);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scroll_steps_preserve_signed_diagonal_totals_and_bound_event_size() {
        for (dx, dy) in [(0, -800), (1, 51), (-51, 13), (0, 1), (10_000, -9_999)] {
            let steps = scroll_steps(dx, dy);
            assert!(steps.len() <= 200);
            assert_eq!(steps.iter().map(|(x, _)| x).sum::<i32>(), dx);
            assert_eq!(steps.iter().map(|(_, y)| y).sum::<i32>(), dy);
            assert!(steps
                .iter()
                .all(|(x, y)| x.abs() <= SCROLL_STEP_PIXELS && y.abs() <= SCROLL_STEP_PIXELS));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_scroll_event_preserves_window_coordinates_and_pixel_direction() {
        let target = WindowTarget {
            window_id: 14461,
            pid: 42,
            x: 0,
            y: 30,
            width: 2560,
            height: 1316,
        };
        for window_id in [14461, 151872] {
            let target = WindowTarget {
                window_id,
                ..target
            };
            let event = mac_scroll_event(Some(target), (1400, 700), 25, -50).unwrap();
            assert_eq!(
                event.get_integer_value_field(CG_EVENT_TARGET_WINDOW),
                i64::from(window_id)
            );
            assert_eq!(
                event.get_integer_value_field(CG_EVENT_RECEIVING_WINDOW),
                i64::from(window_id)
            );
            assert_eq!((event.location().x, event.location().y), (1400.0, 700.0));
            assert_eq!(
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS),
                1
            );
            let foreground = mac_scroll_event(None, (1400, 700), 25, -50).unwrap();
            for (axis, delta) in [
                (EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1, 50),
                (EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2, -25),
            ] {
                assert_eq!(event.get_integer_value_field(axis), delta);
                assert_eq!(foreground.get_integer_value_field(axis), delta);
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires macOS Accessibility permission and launches isolated overlapping test windows"]
    fn macos_background_scroll_targets_one_of_two_windows_without_focus() {
        use std::fs;
        use std::process::{Child, Stdio};

        struct Host(Child);
        impl Drop for Host {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        mac_require_input_permission()
            .expect("grant Accessibility to the test runner before running this ignored test");
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("scroll-host");
        let state_path = temp.path().join("state.json");
        let status = Command::new("xcrun")
            .args(["swiftc", "tests/fixtures/scroll_host.swift", "-o"])
            .arg(&binary)
            .status()
            .unwrap();
        assert!(status.success());
        let _host = Host(
            Command::new(binary)
                .arg(&state_path)
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let read_state =
            || -> Option<Value> { serde_json::from_slice(&fs::read(&state_path).ok()?).ok() };
        let mut initial = None;
        for _ in 0..100 {
            initial = read_state();
            if initial.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let initial = initial.expect("scroll host did not become ready");
        assert_eq!(initial["active"], false);
        let target = WindowTarget {
            window_id: initial["target_id"].as_u64().unwrap() as u32,
            pid: initial["pid"].as_i64().unwrap() as i32,
            x: initial["origin_x"].as_i64().unwrap() as i32,
            y: initial["origin_y"].as_i64().unwrap() as i32,
            width: 0,
            height: 0,
        };
        let point = (
            initial["x"].as_i64().unwrap() as i32,
            initial["y"].as_i64().unwrap() as i32,
        );
        mac_scroll(Some(target), point, 0, 300).unwrap();
        thread::sleep(Duration::from_millis(500));
        let after = read_state().unwrap();
        assert!(
            after["target_offset"].as_f64().unwrap() > initial["target_offset"].as_f64().unwrap(),
            "target did not scroll: {after}"
        );
        assert_eq!(after["decoy_offset"], initial["decoy_offset"]);
        assert_eq!(after["decoy_events"], 0);
        assert_eq!(after["active"], false);
        assert_eq!(after["frontmost_pid"], initial["frontmost_pid"]);
        assert_eq!(after["cursor"], initial["cursor"]);
    }

    #[test]
    fn missing_input_permission_keeps_graphical_computer_use_available() {
        let (gui_available, input_available, reason) =
            capability_status(None, Some("accessibility permission denied".to_string()));
        assert!(gui_available);
        assert!(!input_available);
        assert_eq!(reason.as_deref(), Some("accessibility permission denied"));
    }

    #[test]
    fn computer_use_mode_defaults_to_background_and_rejects_unknown_values() {
        let request: ExecuteRequest = serde_json::from_value(json!({
            "action": { "action": "list_windows" }
        }))
        .unwrap();
        assert_eq!(request.mode, ComputerUseMode::BackgroundApp);
        assert!(serde_json::from_value::<ExecuteRequest>(json!({
            "mode": "automatic",
            "action": { "action": "list_windows" }
        }))
        .is_err());
    }

    #[test]
    fn background_mode_rejects_full_desktop_actions_without_fallback() {
        let request: ExecuteRequest = serde_json::from_value(json!({
            "mode": "background_app",
            "action": { "action": "list_displays" }
        }))
        .unwrap();
        let error = execute(request).unwrap_err();
        assert!(
            error.contains("does not expose the full desktop")
                || error.contains("background_app mode is unavailable")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "launches an isolated TextEdit process to verify real background window input"]
    fn macos_background_window_round_trip_does_not_use_foreground_input() {
        use std::fs;
        use std::io::Write as _;

        let permission_settings = Settings {
            open_prompt_to_get_permissions: false,
            ..Settings::default()
        };
        if Enigo::new(&permission_settings).is_err() {
            eprintln!("skipped: the test binary does not have macOS Accessibility permission");
            return;
        }

        let mut file = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
        file.write_all(b"before").unwrap();
        file.as_file_mut().sync_all().unwrap();
        let path = file.path().to_path_buf();
        let filename = path.file_name().unwrap().to_string_lossy().to_string();

        let status = Command::new("open")
            .args(["-n", "-g", "-a", "TextEdit"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());

        let mut target: Option<(String, WindowTarget)> = None;
        for _ in 0..50 {
            if let Some(found) = Window::all().unwrap().into_iter().find(|window| {
                window
                    .title()
                    .map(|title| title.contains(&filename))
                    .unwrap_or(false)
            }) {
                let id = found.id().unwrap().to_string();
                target = Some((id.clone(), window_target(&id).unwrap()));
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let (window_id, target) = target.expect("isolated TextEdit window did not appear");

        let observe: ComputerAction = serde_json::from_value(json!({
            "action": "observe",
            "window_id": window_id,
        }))
        .unwrap();
        assert!(screenshot(&observe).is_ok());

        let x = target.x + target.width as i32 / 2;
        let y = target.y + target.height as i32 / 2;
        mac_click(target, x, y, Some("left"), 1).unwrap();
        mac_press_keys(target.pid, &["CMD".to_string(), "A".to_string()]).unwrap();
        mac_type_text(target.pid, "background-app-round-trip").unwrap();
        mac_press_keys(target.pid, &["CMD".to_string(), "S".to_string()]).unwrap();

        let mut saved = String::new();
        for _ in 0..30 {
            saved = fs::read_to_string(&path).unwrap_or_default();
            if saved.contains("background-app-round-trip") {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let _ = Command::new("kill").arg(target.pid.to_string()).status();
        assert_eq!(saved, "background-app-round-trip");
    }
}
