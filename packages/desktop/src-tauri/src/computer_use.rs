use base64::{engine::general_purpose::STANDARD, Engine as _};
#[cfg(not(target_os = "macos"))]
use enigo::Axis;
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(target_os = "macos")]
use std::collections::HashSet;
use std::io::Cursor;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use xcap::image::{imageops::FilterType, DynamicImage, ImageFormat, RgbaImage};
use xcap::{Monitor, Window};

#[cfg(target_os = "macos")]
use core_foundation::array::{CFArray, CFArrayRef};
#[cfg(target_os = "macos")]
use core_foundation::base::{CFType, CFTypeRef, TCFType};
#[cfg(target_os = "macos")]
use core_foundation::boolean::CFBoolean;
#[cfg(target_os = "macos")]
use core_foundation::dictionary::{CFDictionary, CFDictionaryGetValue, CFDictionaryRef};
#[cfg(target_os = "macos")]
use core_foundation::number::CFNumber;
#[cfg(target_os = "macos")]
use core_foundation::string::{CFString, CFStringRef};
#[cfg(target_os = "macos")]
use core_graphics::display::CGRectNull;
#[cfg(target_os = "macos")]
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField, KeyCode,
    ScrollEventUnit,
};
#[cfg(target_os = "macos")]
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
#[cfg(target_os = "macos")]
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
#[cfg(target_os = "macos")]
use core_graphics::window::{
    copy_window_info, create_image, kCGNullWindowID, kCGWindowImageDefault,
    kCGWindowListExcludeDesktopElements, kCGWindowListOptionAll,
    kCGWindowListOptionIncludingWindow,
};
#[cfg(target_os = "macos")]
use foreign_types::ForeignType;

const MAX_SCREENSHOT_BYTES: usize = 20 * 1024 * 1024;
const MAX_SCROLL_DELTA: i32 = 10_000;
#[cfg(target_os = "macos")]
const SCROLL_STEP_PIXELS: i32 = 50;
const SCROLL_SETTLE_MS: u64 = 180;
#[cfg(target_os = "macos")]
const CLICK_SETTLE_MS: u64 = 250;

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

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct MacWindowInfo {
    target: WindowTarget,
    title: String,
    layer: i32,
    on_screen: bool,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct MacWindowGroup {
    // Components are ordered front-to-back and always include the root.
    components: Vec<MacWindowInfo>,
}

#[cfg(target_os = "macos")]
fn mac_dictionary_value(dictionary: &CFDictionary, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    let value = unsafe {
        CFDictionaryGetValue(dictionary.as_concrete_TypeRef(), key.as_CFTypeRef().cast())
    };
    if value.is_null() {
        None
    } else {
        Some(unsafe { CFType::wrap_under_get_rule(value.cast()) })
    }
}

#[cfg(target_os = "macos")]
fn mac_dictionary_number(dictionary: &CFDictionary, key: &str) -> Option<i64> {
    mac_dictionary_value(dictionary, key)?
        .downcast::<CFNumber>()?
        .to_i64()
}

#[cfg(target_os = "macos")]
fn mac_dictionary_string(dictionary: &CFDictionary, key: &str) -> Option<String> {
    mac_dictionary_value(dictionary, key)
        .and_then(|value| value.downcast::<CFString>())
        .map(|value| value.to_string())
}

#[cfg(target_os = "macos")]
fn mac_dictionary_bool(dictionary: &CFDictionary, key: &str) -> Option<bool> {
    mac_dictionary_value(dictionary, key)
        .and_then(|value| value.downcast::<CFBoolean>())
        .map(|value| value == CFBoolean::true_value())
}

#[cfg(target_os = "macos")]
fn mac_all_window_info() -> Result<Vec<MacWindowInfo>, String> {
    let windows = copy_window_info(
        kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )
    .ok_or_else(|| "Unable to enumerate macOS windows".to_string())?;
    let mut result = Vec::new();
    for raw in windows.get_all_values() {
        if raw.is_null() {
            continue;
        }
        let dictionary = unsafe {
            CFDictionary::wrap_under_get_rule(raw.cast::<std::ffi::c_void>() as CFDictionaryRef)
        };
        let Some(window_id) = mac_dictionary_number(&dictionary, "kCGWindowNumber")
            .and_then(|value| u32::try_from(value).ok())
        else {
            continue;
        };
        let Some(pid) = mac_dictionary_number(&dictionary, "kCGWindowOwnerPID")
            .and_then(|value| i32::try_from(value).ok())
        else {
            continue;
        };
        if mac_dictionary_number(&dictionary, "kCGWindowSharingState") == Some(0) {
            continue;
        }
        let Some(bounds) = mac_dictionary_value(&dictionary, "kCGWindowBounds")
            .and_then(|value| value.downcast::<CFDictionary>())
            .and_then(|value| CGRect::from_dict_representation(&value))
        else {
            continue;
        };
        if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
            continue;
        }
        result.push(MacWindowInfo {
            target: WindowTarget {
                window_id,
                pid,
                x: bounds.origin.x.round() as i32,
                y: bounds.origin.y.round() as i32,
                width: bounds.size.width.round() as u32,
                height: bounds.size.height.round() as u32,
            },
            title: mac_dictionary_string(&dictionary, "kCGWindowName").unwrap_or_default(),
            layer: mac_dictionary_number(&dictionary, "kCGWindowLayer")
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or_default(),
            on_screen: mac_dictionary_bool(&dictionary, "kCGWindowIsOnscreen").unwrap_or(false),
        });
    }
    Ok(result)
}

#[cfg(target_os = "macos")]
fn window_intersection_area(left: WindowTarget, right: WindowTarget) -> u64 {
    let left_edge = left.x.max(right.x) as i64;
    let top_edge = left.y.max(right.y) as i64;
    let right_edge = (i64::from(left.x) + i64::from(left.width))
        .min(i64::from(right.x) + i64::from(right.width));
    let bottom_edge = (i64::from(left.y) + i64::from(left.height))
        .min(i64::from(right.y) + i64::from(right.height));
    if right_edge <= left_edge || bottom_edge <= top_edge {
        0
    } else {
        (right_edge - left_edge) as u64 * (bottom_edge - top_edge) as u64
    }
}

#[cfg(target_os = "macos")]
fn same_window_bounds(left: WindowTarget, right: WindowTarget) -> bool {
    left.x == right.x
        && left.y == right.y
        && left.width == right.width
        && left.height == right.height
}

#[cfg(target_os = "macos")]
fn mac_window_group_from_info(
    root: WindowTarget,
    windows: &[MacWindowInfo],
    accessibility_window_ids: &HashSet<u32>,
) -> MacWindowGroup {
    let Some(root_index) = windows
        .iter()
        .position(|window| window.target.window_id == root.window_id)
    else {
        return MacWindowGroup {
            components: vec![MacWindowInfo {
                target: root,
                title: String::new(),
                layer: 0,
                on_screen: true,
            }],
        };
    };
    let root_layer = windows[root_index].layer;
    let root_area = u64::from(root.width) * u64::from(root.height);
    let mut components = windows[..=root_index]
        .iter()
        .filter(|window| {
            if window.target.window_id == root.window_id {
                return true;
            }
            if window.target.pid != root.pid || window.layer != root_layer {
                return false;
            }
            let intersection = window_intersection_area(window.target, root);
            if intersection == 0 {
                return false;
            }
            if window.on_screen {
                // Electron apps often keep a full-size, titleless watermark or
                // hit-test companion above the real window. It is not an
                // interaction surface and must not steal routed events.
                return !(window.title.is_empty() && same_window_bounds(window.target, root));
            }
            // Electron may retain a closed transient window indefinitely in
            // the CoreGraphics list, including its stale backing store. AXWindows
            // reflects the application's current logical windows, so require
            // that independent signal before reviving an ordered-out surface.
            // If Accessibility cannot enumerate the application, hidden windows
            // are omitted instead of risking a false overlay or unsafe routing.
            accessibility_window_ids.contains(&window.target.window_id)
                && root_area > 0
                && intersection.saturating_mul(50) >= root_area
        })
        .cloned()
        .collect::<Vec<_>>();
    if !components
        .iter()
        .any(|window| window.target.window_id == root.window_id)
    {
        components.push(windows[root_index].clone());
    }
    MacWindowGroup { components }
}

#[cfg(target_os = "macos")]
fn mac_window_group(root: WindowTarget) -> Result<MacWindowGroup, String> {
    let windows = mac_all_window_info()?;
    let accessibility_window_ids = mac_ax_application_window_ids(root.pid).unwrap_or_default();
    Ok(mac_window_group_from_info(
        root,
        &windows,
        &accessibility_window_ids,
    ))
}

#[cfg(target_os = "macos")]
fn point_in_window(target: WindowTarget, point: (i32, i32)) -> bool {
    i64::from(point.0) >= i64::from(target.x)
        && i64::from(point.1) >= i64::from(target.y)
        && i64::from(point.0) < i64::from(target.x) + i64::from(target.width)
        && i64::from(point.1) < i64::from(target.y) + i64::from(target.height)
}

#[cfg(target_os = "macos")]
fn mac_event_target_from_group(
    group: MacWindowGroup,
    root: WindowTarget,
    point: (i32, i32),
) -> MacWindowInfo {
    group
        .components
        .into_iter()
        .find(|window| point_in_window(window.target, point))
        .unwrap_or(MacWindowInfo {
            target: root,
            title: String::new(),
            layer: 0,
            on_screen: true,
        })
}

#[cfg(target_os = "macos")]
fn mac_background_event_target(root: WindowTarget, point: (i32, i32)) -> MacWindowInfo {
    mac_window_group(root)
        .ok()
        .map(|group| mac_event_target_from_group(group, root, point))
        .unwrap_or(MacWindowInfo {
            target: root,
            title: String::new(),
            layer: 0,
            on_screen: true,
        })
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

fn background_local_point(
    action: &ComputerAction,
    target: WindowTarget,
) -> Result<(i32, i32), String> {
    let x = required(action.x, "x")?;
    let y = required(action.y, "y")?;
    if x < 0 || y < 0 || x as u32 >= target.width || y as u32 >= target.height {
        return Err(format!(
            "Point ({x}, {y}) is outside target window-local bounds 0,0 {}x{}",
            target.width, target.height
        ));
    }
    Ok((x, y))
}

fn background_point(action: &ComputerAction, target: WindowTarget) -> Result<(i32, i32), String> {
    let (x, y) = background_local_point(action, target)?;
    Ok((
        target
            .x
            .checked_add(x)
            .ok_or_else(|| "Background X coordinate overflowed desktop space".to_string())?,
        target
            .y
            .checked_add(y)
            .ok_or_else(|| "Background Y coordinate overflowed desktop space".to_string())?,
    ))
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
type AXError = i32;

#[cfg(target_os = "macos")]
const AX_ERROR_SUCCESS: AXError = 0;
#[cfg(target_os = "macos")]
const AX_ERROR_CANNOT_COMPLETE: AXError = -25204;
#[cfg(target_os = "macos")]
const AX_ERROR_ACTION_UNSUPPORTED: AXError = -25206;

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: libc::pid_t) -> CFTypeRef;
    fn AXUIElementCopyElementAtPosition(
        application: CFTypeRef,
        x: f32,
        y: f32,
        element: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementCopyAttributeValue(
        element: CFTypeRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementCopyActionNames(element: CFTypeRef, actions: *mut CFArrayRef) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: CFTypeRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementPerformAction(element: CFTypeRef, action: CFStringRef) -> AXError;
    fn AXUIElementGetPid(element: CFTypeRef, pid: *mut libc::pid_t) -> AXError;
    fn AXValueGetType(value: CFTypeRef) -> u32;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetValue(value: CFTypeRef, value_type: u32, output: *mut libc::c_void) -> bool;
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
enum MacAxPressOutcome {
    Performed { action: &'static str },
    Unsupported { reason: String },
    Uncertain { reason: String },
}

#[cfg(target_os = "macos")]
fn mac_ax_error(operation: &str, status: AXError) -> String {
    format!("{operation} failed with AXError {status}")
}

#[cfg(target_os = "macos")]
fn mac_ax_copy_attribute(element: &CFType, attribute: &str) -> Result<CFType, AXError> {
    let attribute = CFString::new(attribute);
    let mut value_ref: CFTypeRef = std::ptr::null();
    let status = unsafe {
        AXUIElementCopyAttributeValue(
            element.as_CFTypeRef(),
            attribute.as_concrete_TypeRef(),
            &mut value_ref,
        )
    };
    if status != AX_ERROR_SUCCESS || value_ref.is_null() {
        return Err(status);
    }
    Ok(unsafe { CFType::wrap_under_create_rule(value_ref) })
}

#[cfg(target_os = "macos")]
fn mac_ax_supports_action(element: &CFType, action: &str) -> Result<bool, AXError> {
    let mut actions_ref: CFArrayRef = std::ptr::null();
    let status = unsafe { AXUIElementCopyActionNames(element.as_CFTypeRef(), &mut actions_ref) };
    if status != AX_ERROR_SUCCESS || actions_ref.is_null() {
        return Err(status);
    }
    let actions = unsafe { CFArray::<CFString>::wrap_under_create_rule(actions_ref) };
    Ok(actions
        .iter()
        .any(|candidate| candidate.to_string() == action))
}

#[cfg(target_os = "macos")]
fn mac_ax_window_id(element: &CFType) -> Result<u32, String> {
    type GetWindow = unsafe extern "C" fn(CFTypeRef, *mut u32) -> AXError;
    static GET_WINDOW: std::sync::OnceLock<Option<GetWindow>> = std::sync::OnceLock::new();
    let get_window = GET_WINDOW.get_or_init(|| {
        for name in [c"_AXUIElementGetWindow", c"AXUIElementGetWindow"] {
            let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            if !symbol.is_null() {
                return Some(unsafe {
                    std::mem::transmute::<*mut libc::c_void, GetWindow>(symbol)
                });
            }
        }
        None
    });
    if let Some(get_window) = get_window {
        let mut window_id = 0;
        let status = unsafe { get_window(element.as_CFTypeRef(), &mut window_id) };
        if status == AX_ERROR_SUCCESS && window_id != 0 {
            return Ok(window_id);
        }
    }

    let window = mac_ax_copy_attribute(element, "AXWindow")
        .map_err(|status| mac_ax_error("Reading AXWindow", status))?;
    // AXWindowNumber is supplied by the macOS accessibility server. Matching
    // it prevents a hit test from acting on another overlapping window owned
    // by the same Electron process.
    let number = mac_ax_copy_attribute(&window, "AXWindowNumber")
        .map_err(|status| mac_ax_error("Reading AXWindowNumber", status))?
        .downcast::<CFNumber>()
        .ok_or_else(|| "AXWindowNumber was not numeric".to_string())?;
    let value = number
        .to_i64()
        .ok_or_else(|| "AXWindowNumber was outside the supported range".to_string())?;
    u32::try_from(value).map_err(|_| "AXWindowNumber was outside the supported range".to_string())
}

#[cfg(target_os = "macos")]
fn mac_ax_application_window_ids(pid: i32) -> Result<HashSet<u32>, String> {
    let application_ref = unsafe { AXUIElementCreateApplication(pid) };
    if application_ref.is_null() {
        return Err("Unable to create the target application's accessibility element".to_string());
    }
    let application = unsafe { CFType::wrap_under_create_rule(application_ref) };
    let windows = mac_ax_copy_attribute(&application, "AXWindows")
        .map_err(|status| mac_ax_error("Reading AXWindows", status))?
        .downcast::<CFArray>()
        .ok_or_else(|| "AXWindows was not an accessibility element array".to_string())?;
    Ok(windows
        .get_all_values()
        .into_iter()
        .filter(|window| !window.is_null())
        .filter_map(|window| {
            let window = unsafe { CFType::wrap_under_get_rule(window.cast()) };
            mac_ax_window_id(&window).ok()
        })
        .collect())
}

#[cfg(target_os = "macos")]
fn mac_ax_contains_point(element: &CFType, point: CGPoint) -> Option<bool> {
    let position = mac_ax_copy_attribute(element, "AXPosition").ok()?;
    let size = mac_ax_copy_attribute(element, "AXSize").ok()?;
    let mut origin = CGPoint::new(0.0, 0.0);
    let mut dimensions = CGSize::new(0.0, 0.0);
    // AXValue's public CGPoint/CGSize type IDs are 1 and 2 respectively.
    if position.type_of() != unsafe { AXValueGetTypeID() }
        || size.type_of() != unsafe { AXValueGetTypeID() }
        || unsafe { AXValueGetType(position.as_CFTypeRef()) } != 1
        || unsafe { AXValueGetType(size.as_CFTypeRef()) } != 2
        || !unsafe {
            AXValueGetValue(
                position.as_CFTypeRef(),
                1,
                (&mut origin as *mut CGPoint).cast(),
            )
        }
        || !unsafe {
            AXValueGetValue(
                size.as_CFTypeRef(),
                2,
                (&mut dimensions as *mut CGSize).cast(),
            )
        }
    {
        return None;
    }
    Some(
        dimensions.width > 0.0
            && dimensions.height > 0.0
            && point.x >= origin.x
            && point.y >= origin.y
            && point.x < origin.x + dimensions.width
            && point.y < origin.y + dimensions.height,
    )
}

#[cfg(target_os = "macos")]
fn mac_ax_pressable_at_point(
    element: CFType,
    target: WindowTarget,
    point: CGPoint,
    depth: usize,
    remaining: &mut usize,
) -> Option<CFType> {
    if depth > 32 || *remaining == 0 {
        return None;
    }
    *remaining -= 1;
    for (attribute, unavailable) in [("AXHidden", true), ("AXEnabled", false)] {
        if mac_ax_copy_attribute(&element, attribute)
            .ok()
            .and_then(|value| value.downcast::<CFBoolean>())
            .map(bool::from)
            == Some(unavailable)
        {
            return None;
        }
    }
    let contains = mac_ax_contains_point(&element, point);
    if contains == Some(false) {
        return None;
    }
    if let Ok(children) = mac_ax_copy_attribute(&element, "AXChildren") {
        if let Some(children) = children.downcast::<CFArray>() {
            for child in children.get_all_values().into_iter().rev() {
                if child.is_null() {
                    continue;
                }
                let child = unsafe { CFType::wrap_under_get_rule(child.cast()) };
                if let Some(hit) =
                    mac_ax_pressable_at_point(child, target, point, depth + 1, remaining)
                {
                    return Some(hit);
                }
            }
        }
    }
    // Unknown geometry may belong to a container; traverse it but never
    // invoke an action on it. Keep the final candidate in the selected window.
    if contains == Some(true)
        && mac_ax_window_id(&element) == Ok(target.window_id)
        && (mac_ax_supports_action(&element, "AXPress") == Ok(true)
            || mac_ax_supports_action(&element, "AXPick") == Ok(true))
    {
        Some(element)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn mac_ax_window_hit(
    application: &CFType,
    target: WindowTarget,
    x: i32,
    y: i32,
) -> Result<CFType, String> {
    let windows = mac_ax_copy_attribute(application, "AXWindows")
        .map_err(|status| mac_ax_error("Reading AXWindows for click", status))?
        .downcast::<CFArray>()
        .ok_or_else(|| "AXWindows was not an accessibility element array".to_string())?;
    for window in windows.get_all_values() {
        if window.is_null() {
            continue;
        }
        let window = unsafe { CFType::wrap_under_get_rule(window.cast()) };
        if mac_ax_window_id(&window) != Ok(target.window_id) {
            continue;
        }
        // Application-wide AX hit testing follows window z-order. For example,
        // Lark's watermark companion obscures the real controls in that API.
        // Search only the requested window's AX tree; never click the companion
        // or activate the app to make the global hit test work.
        let mut remaining = 1024;
        return mac_ax_pressable_at_point(
            window,
            target,
            CGPoint::new(f64::from(x), f64::from(y)),
            0,
            &mut remaining,
        )
        .ok_or_else(|| "No background accessibility click action at the target point".to_string());
    }
    Err("The selected window is absent from the application's accessibility tree".to_string())
}

#[cfg(target_os = "macos")]
fn mac_ax_press(target: WindowTarget, x: i32, y: i32) -> MacAxPressOutcome {
    let application_ref = unsafe { AXUIElementCreateApplication(target.pid) };
    if application_ref.is_null() {
        return MacAxPressOutcome::Unsupported {
            reason: "Unable to create the target application's accessibility element".to_string(),
        };
    }
    let application = unsafe { CFType::wrap_under_create_rule(application_ref) };

    // Chromium/Electron applications may not expose their accessibility tree
    // until an assistive client opts in. This does not activate, focus, or
    // raise the application.
    let manual_accessibility = CFString::new("AXManualAccessibility");
    let enabled = CFBoolean::true_value();
    let manual_status = unsafe {
        AXUIElementSetAttributeValue(
            application.as_CFTypeRef(),
            manual_accessibility.as_concrete_TypeRef(),
            enabled.as_CFTypeRef(),
        )
    };
    if manual_status == AX_ERROR_SUCCESS {
        thread::sleep(Duration::from_millis(50));
    }

    let mut hit_ref: CFTypeRef = std::ptr::null();
    let hit_status = unsafe {
        AXUIElementCopyElementAtPosition(
            application.as_CFTypeRef(),
            x as f32,
            y as f32,
            &mut hit_ref,
        )
    };
    let parent_attribute = CFString::new("AXParent");
    let press_action = CFString::new("AXPress");
    let pick_action = CFString::new("AXPick");
    let hit = (!hit_ref.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(hit_ref) });
    let mut current = match hit.filter(|hit| {
        hit_status == AX_ERROR_SUCCESS && mac_ax_window_id(hit) == Ok(target.window_id)
    }) {
        Some(hit) => hit,
        None => match mac_ax_window_hit(&application, target, x, y) {
            Ok(hit) => hit,
            Err(reason) => return MacAxPressOutcome::Unsupported { reason },
        },
    };
    for _ in 0..16 {
        let mut element_pid = 0;
        let pid_status = unsafe { AXUIElementGetPid(current.as_CFTypeRef(), &mut element_pid) };
        if pid_status != AX_ERROR_SUCCESS || element_pid != target.pid {
            return MacAxPressOutcome::Unsupported {
                reason: "Accessibility hit test escaped the target application".to_string(),
            };
        }
        if mac_ax_window_id(&current) != Ok(target.window_id) {
            break;
        }

        for (name, action) in [("AXPress", &press_action), ("AXPick", &pick_action)] {
            // Some Chromium groups return success for arbitrary AX actions
            // even though their advertised action list contains only
            // AXShowMenu/AXScrollToVisible. Treat those no-op acknowledgements
            // as unsupported. A raw mouse fallback can activate the app.
            if mac_ax_supports_action(&current, name) != Ok(true) {
                continue;
            }
            let status = unsafe {
                AXUIElementPerformAction(current.as_CFTypeRef(), action.as_concrete_TypeRef())
            };
            match status {
                AX_ERROR_SUCCESS => return MacAxPressOutcome::Performed { action: name },
                AX_ERROR_ACTION_UNSUPPORTED => {}
                AX_ERROR_CANNOT_COMPLETE => {
                    // Apple documents that CannotComplete may still mean the
                    // application handled the action. Do not risk a duplicate
                    // click through the Quartz fallback.
                    return MacAxPressOutcome::Uncertain {
                        reason: mac_ax_error(name, status),
                    };
                }
                _ => {
                    return MacAxPressOutcome::Uncertain {
                        reason: mac_ax_error(name, status),
                    };
                }
            }
        }

        let mut parent_ref: CFTypeRef = std::ptr::null();
        let parent_status = unsafe {
            AXUIElementCopyAttributeValue(
                current.as_CFTypeRef(),
                parent_attribute.as_concrete_TypeRef(),
                &mut parent_ref,
            )
        };
        if parent_status != AX_ERROR_SUCCESS || parent_ref.is_null() {
            break;
        }
        current = unsafe { CFType::wrap_under_create_rule(parent_ref) };
    }

    MacAxPressOutcome::Unsupported {
        reason: "No accessibility element at the point supports AXPress or AXPick".to_string(),
    }
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
        "Window-targeted background input is unavailable on this macOS version".to_string()
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
type MacGetProcessForPid = unsafe extern "C" fn(i32, *mut u32) -> i32;
#[cfg(target_os = "macos")]
type MacGetFrontProcess = unsafe extern "C" fn(*mut u32) -> i32;

#[cfg(target_os = "macos")]
fn mac_process_api() -> Result<(MacGetProcessForPid, MacGetFrontProcess), String> {
    static API: std::sync::OnceLock<Option<(MacGetProcessForPid, MacGetFrontProcess)>> =
        std::sync::OnceLock::new();
    API.get_or_init(|| unsafe {
        let get = libc::dlsym(libc::RTLD_DEFAULT, c"GetProcessForPID".as_ptr());
        let front = libc::dlsym(libc::RTLD_DEFAULT, c"_SLPSGetFrontProcess".as_ptr());
        if get.is_null() || front.is_null() {
            None
        } else {
            Some((
                std::mem::transmute::<*mut libc::c_void, MacGetProcessForPid>(get),
                std::mem::transmute::<*mut libc::c_void, MacGetFrontProcess>(front),
            ))
        }
    })
    .as_ref()
    .copied()
    .ok_or_else(|| "macOS foreground verification is unavailable".to_string())
}

#[cfg(target_os = "macos")]
fn mac_process_serial_number(pid: i32) -> Result<[u32; 2], String> {
    let (get, _) = mac_process_api()?;
    let mut process = [0u32; 2];
    if unsafe { get(pid, process.as_mut_ptr()) } != 0 {
        return Err(format!("Unable to resolve process identity for PID {pid}"));
    }
    Ok(process)
}

#[cfg(target_os = "macos")]
fn mac_front_process_serial_number() -> Result<[u32; 2], String> {
    let (_, front) = mac_process_api()?;
    let mut process = [0u32; 2];
    if unsafe { front(process.as_mut_ptr()) } != 0 {
        return Err("Unable to read the macOS foreground process".to_string());
    }
    Ok(process)
}

#[cfg(target_os = "macos")]
struct MacForegroundMonitor {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    activated: std::sync::Arc<std::sync::atomic::AtomicBool>,
    verification_failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
impl MacForegroundMonitor {
    fn start(target_pid: i32) -> Result<Self, String> {
        use std::sync::atomic::Ordering;

        let target = mac_process_serial_number(target_pid)?;
        let initial = mac_front_process_serial_number()?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let activated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let verification_failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = if initial == target {
            None
        } else {
            let thread_stop = stop.clone();
            let thread_activated = activated.clone();
            let thread_failed = verification_failed.clone();
            Some(thread::spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    match mac_front_process_serial_number() {
                        Ok(front) if front == target => {
                            thread_activated.store(true, Ordering::Release);
                        }
                        Ok(_) => {}
                        Err(_) => {
                            thread_failed.store(true, Ordering::Release);
                            break;
                        }
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            }))
        };
        Ok(Self {
            stop,
            activated,
            verification_failed,
            handle,
        })
    }

    fn finish(mut self) -> Result<bool, String> {
        use std::sync::atomic::Ordering;

        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| "Foreground verification thread failed".to_string())?;
        }
        if self.verification_failed.load(Ordering::Acquire) {
            return Err("macOS foreground verification failed during input".to_string());
        }
        Ok(self.activated.load(Ordering::Acquire))
    }
}

#[cfg(target_os = "macos")]
impl Drop for MacForegroundMonitor {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;

        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(target_os = "macos")]
fn mac_mouse_event(
    target: WindowTarget,
    event_type: CGEventType,
    button: CGMouseButton,
    x: i32,
    y: i32,
    click_count: i64,
) -> Result<CGEvent, String> {
    let event = CGEvent::new_mouse_event(
        mac_event_source()?,
        event_type,
        CGPoint::new(x as f64, y as f64),
        button,
    )
    .map_err(|_| "Unable to create a macOS mouse event".to_string())?;
    event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_count);
    mac_target_pointer_event(&event, target)?;
    Ok(event)
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
    mac_mouse_event(target, event_type, button, x, y, click_count)?.post_to_pid(target.pid);
    Ok(())
}

#[cfg(target_os = "macos")]
fn mac_post_prepared_mouse(event: &CGEvent, pid: i32) {
    unsafe extern "C" {
        fn CGEventSetTimestamp(event: core_graphics::sys::CGEventRef, timestamp: u64);
        fn clock_gettime_nsec_np(clock_id: libc::clockid_t) -> u64;
    }
    // Prepared drag/double-click events must reflect dispatch time, not their
    // shared construction time. Quartz timestamps use uptime in nanoseconds.
    unsafe {
        CGEventSetTimestamp(
            event.as_ptr(),
            clock_gettime_nsec_np(libc::CLOCK_UPTIME_RAW),
        );
    }
    event.post_to_pid(pid);
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
    // Chromium-based applications use the preceding pointer location for hit
    // testing and hover state. Send a target-local move before mouse-down; the
    // event is posted only to the target PID and does not move the real cursor.
    let moved = mac_mouse_event(
        target,
        CGEventType::MouseMoved,
        CGMouseButton::Left,
        x,
        y,
        0,
    )?;
    // Construct every event before sending mouse-down. Do not synthesize an
    // application activation record: Electron treats that private event as a
    // real foreground request even when WindowServer initially leaves the
    // process in the background.
    let events = (1..=click_count)
        .map(|count| {
            Ok((
                mac_mouse_event(target, down, button, x, y, count)?,
                mac_mouse_event(target, up, button, x, y, count)?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    mac_post_prepared_mouse(&moved, target.pid);
    thread::sleep(Duration::from_millis(12));
    for (index, (down, up)) in events.iter().enumerate() {
        mac_post_prepared_mouse(down, target.pid);
        thread::sleep(Duration::from_millis(20));
        mac_post_prepared_mouse(up, target.pid);
        if index + 1 < events.len() {
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
    let down = mac_mouse_event(target, down, button, start.0, start.1, 1)?;
    let up = mac_mouse_event(target, up, button, end.0, end.1, 1)?;
    let steps = 20i32;
    let motion = (1..=steps)
        .map(|step| {
            let x = start.0 + (end.0 - start.0) * step / steps;
            let y = start.1 + (end.1 - start.1) * step / steps;
            mac_mouse_event(target, dragged, button, x, y, 1)
        })
        .collect::<Result<Vec<_>, _>>()?;
    mac_post_prepared_mouse(&down, target.pid);
    for event in motion {
        mac_post_prepared_mouse(&event, target.pid);
        thread::sleep(Duration::from_millis(duration_ms / steps as u64));
    }
    mac_post_prepared_mouse(&up, target.pid);
    Ok(())
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

struct ScreenshotCapture {
    image: RgbaImage,
    origin_x: i32,
    origin_y: i32,
    target: String,
    component_window_ids: Vec<u32>,
    hidden_component_window_ids: Vec<u32>,
    component_capture_errors: Vec<String>,
}

#[cfg(target_os = "macos")]
fn mac_capture_window(target: WindowTarget) -> Result<RgbaImage, String> {
    let bounds = CGRect::new(
        &CGPoint::new(f64::from(target.x), f64::from(target.y)),
        &CGSize::new(f64::from(target.width), f64::from(target.height)),
    );
    let Some(image) = create_image(
        bounds,
        kCGWindowListOptionIncludingWindow,
        target.window_id,
        kCGWindowImageDefault,
    )
    .or_else(|| {
        create_image(
            unsafe { CGRectNull },
            kCGWindowListOptionIncludingWindow,
            target.window_id,
            kCGWindowImageDefault,
        )
    }) else {
        return mac_capture_hidden_window_with_screencapture(target);
    };
    let width = image.width();
    let height = image.height();
    let bytes_per_row = image.bytes_per_row();
    if width == 0 || height == 0 || bytes_per_row < width.saturating_mul(4) {
        return Err(format!(
            "Window {} returned an invalid capture buffer",
            target.window_id
        ));
    }
    let data = image.data();
    let bytes = data.bytes();
    if bytes.len() < bytes_per_row.saturating_mul(height) {
        return Err(format!(
            "Window {} returned a truncated capture buffer",
            target.window_id
        ));
    }
    let mut rgba = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4));
    for row in bytes.chunks_exact(bytes_per_row).take(height) {
        rgba.extend_from_slice(&row[..width * 4]);
    }
    for bgra in rgba.chunks_exact_mut(4) {
        bgra.swap(0, 2);
    }
    let image = RgbaImage::from_raw(width as u32, height as u32, rgba)
        .ok_or_else(|| format!("Unable to decode window {} capture", target.window_id))?;
    Ok(normalize_image(image, target.width, target.height))
}

#[cfg(target_os = "macos")]
fn mac_capture_hidden_window_with_screencapture(target: WindowTarget) -> Result<RgbaImage, String> {
    let output = tempfile::Builder::new()
        .prefix("crabcode-window-")
        .suffix(".png")
        .tempfile()
        .map_err(|error| format!("Unable to prepare hidden-window capture: {error}"))?;
    let path = output.path();
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-o", "-l", &target.window_id.to_string()])
        .arg(path)
        .status()
        .map_err(|error| {
            format!(
                "Unable to invoke hidden-window capture for {}: {error}",
                target.window_id
            )
        })?;
    if !status.success() {
        return Err(format!(
            "Unable to capture hidden window {}; screencapture exited with {status}",
            target.window_id
        ));
    }
    let image = xcap::image::open(path)
        .map_err(|error| {
            format!(
                "Unable to decode hidden window {}: {error}",
                target.window_id
            )
        })?
        .to_rgba8();
    Ok(normalize_image(image, target.width, target.height))
}

#[cfg(target_os = "macos")]
fn mac_capture_window_group(root: WindowTarget) -> Result<ScreenshotCapture, String> {
    let group = mac_window_group(root)?;
    let mut image = mac_capture_window(root)?;
    let mut component_window_ids = vec![root.window_id];
    let mut hidden_component_window_ids = Vec::new();
    let mut component_capture_errors = Vec::new();

    // The Core Graphics list is front-to-back. Composite from the root toward
    // the front so each higher transient surface lands above its owner.
    for component in group.components.iter().rev() {
        if component.target.window_id == root.window_id {
            continue;
        }
        if !component.on_screen {
            hidden_component_window_ids.push(component.target.window_id);
        }
        match mac_capture_window(component.target) {
            Ok(overlay) => {
                let offset_x = i64::from(component.target.x) - i64::from(root.x);
                let offset_y = i64::from(component.target.y) - i64::from(root.y);
                xcap::image::imageops::overlay(&mut image, &overlay, offset_x, offset_y);
                component_window_ids.push(component.target.window_id);
            }
            Err(error) => component_capture_errors.push(error),
        }
    }

    Ok(ScreenshotCapture {
        image,
        origin_x: root.x,
        origin_y: root.y,
        target: format!("window:{}", root.window_id),
        component_window_ids,
        hidden_component_window_ids,
        component_capture_errors,
    })
}

fn capture_screenshot(action: &ComputerAction) -> Result<ScreenshotCapture, String> {
    let (image, x, y, target) = if let Some(window_id) = action.window_id.as_deref() {
        #[cfg(target_os = "macos")]
        {
            return mac_capture_window_group(window_target(window_id)?);
        }

        #[cfg(not(target_os = "macos"))]
        {
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
        }
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
    Ok(ScreenshotCapture {
        image,
        origin_x: x,
        origin_y: y,
        target,
        component_window_ids: Vec::new(),
        hidden_component_window_ids: Vec::new(),
        component_capture_errors: Vec::new(),
    })
}

fn screenshot(action: &ComputerAction) -> Result<Value, String> {
    encode_screenshot_capture(capture_screenshot(action)?)
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

fn encode_screenshot_capture(capture: ScreenshotCapture) -> Result<Value, String> {
    let mut frame = encode_screenshot(
        capture.image,
        capture.origin_x,
        capture.origin_y,
        capture.target,
    )?;
    if !capture.component_window_ids.is_empty() {
        frame["component_window_ids"] = json!(capture.component_window_ids);
    }
    if !capture.hidden_component_window_ids.is_empty() {
        frame["hidden_component_window_ids"] = json!(capture.hidden_component_window_ids);
    }
    if !capture.component_capture_errors.is_empty() {
        frame["component_capture_errors"] = json!(capture.component_capture_errors);
    }
    if !frame["hidden_component_window_ids"].is_null()
        || !frame["component_capture_errors"].is_null()
    {
        frame["background_observation_limited"] = Value::Bool(true);
        frame["observation_warning"] = Value::String(
            "The application has an auxiliary window whose background pixels may be incomplete or unavailable"
                .to_string(),
        );
    }
    Ok(frame)
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
#[derive(Debug, Clone, Copy)]
struct ScreenshotVisualChange {
    detected: bool,
    changed_pixels: u32,
    sampled_pixels: u32,
}

#[cfg(target_os = "macos")]
fn screenshot_visual_change(
    before: &ScreenshotCapture,
    after: &ScreenshotCapture,
    point: (i32, i32),
) -> Option<ScreenshotVisualChange> {
    if before.origin_x != after.origin_x
        || before.origin_y != after.origin_y
        || before.target != after.target
        || before.image.dimensions() != after.image.dimensions()
    {
        return None;
    }
    let (width, height) = before.image.dimensions();
    let local_x = point.0.checked_sub(before.origin_x)?;
    let local_y = point.1.checked_sub(before.origin_y)?;
    if local_x < 0 || local_y < 0 || local_x >= width as i32 || local_y >= height as i32 {
        return None;
    }

    // Verify only the clicked neighbourhood. Whole-window comparison produced
    // false positives in live chat applications when an unrelated badge or
    // timestamp changed while the intended control ignored the click.
    const RADIUS: i32 = 64;
    const MIN_CHANGED_PIXELS: u32 = 24;
    const MIN_CHANNEL_DELTA: u16 = 48;
    let left = (local_x - RADIUS).max(0) as u32;
    let right = (local_x + RADIUS + 1).min(width as i32) as u32;
    let top = (local_y - RADIUS).max(0) as u32;
    let bottom = (local_y + RADIUS + 1).min(height as i32) as u32;
    let mut changed_pixels = 0u32;
    for pixel_y in top..bottom {
        for pixel_x in left..right {
            let before_pixel = before.image.get_pixel(pixel_x, pixel_y);
            let after_pixel = after.image.get_pixel(pixel_x, pixel_y);
            let delta = before_pixel
                .0
                .iter()
                .zip(after_pixel.0.iter())
                .take(3)
                .map(|(before, after)| u16::from(before.abs_diff(*after)))
                .sum::<u16>();
            if delta >= MIN_CHANNEL_DELTA {
                changed_pixels += 1;
            }
        }
    }
    let sampled_pixels = (right - left) * (bottom - top);
    Some(ScreenshotVisualChange {
        detected: changed_pixels >= MIN_CHANGED_PIXELS,
        changed_pixels,
        sampled_pixels,
    })
}

#[cfg(target_os = "macos")]
fn finish_background_click(
    action: &ComputerAction,
    mut result: Value,
    screenshot: Result<ScreenshotCapture, String>,
) -> Value {
    match screenshot {
        Ok(capture) => {
            if action.include_screenshot.unwrap_or(true) {
                match encode_screenshot_capture(capture) {
                    Ok(frame) => {
                        if frame["background_observation_limited"] == Value::Bool(true) {
                            result["background_observation_limited"] = Value::Bool(true);
                            result["observation_warning"] = frame["observation_warning"].clone();
                        }
                        result["screenshot"] = frame;
                    }
                    Err(error) => result["screenshot_error"] = Value::String(error),
                }
            }
        }
        Err(error) => {
            result["screenshot_error"] = Value::String(error);
        }
    }
    result
}

#[cfg(target_os = "macos")]
fn unsupported_background_click(
    action: &ComputerAction,
    target: WindowTarget,
    event_target: WindowTarget,
    point: (i32, i32),
    reason: String,
) -> Value {
    json!({
        "ok": false,
        "mode": "background_app",
        "action": action.action,
        "summary": "Background click is unsupported by the target control",
        "error_code": "background_click_unsupported",
        "error": reason,
        "cursor": { "x": point.0, "y": point.1 },
        "coordinate_space": "window",
        "window_origin": { "x": target.x, "y": target.y },
        "dispatch_window_id": event_target.window_id.to_string(),
        "dispatch_succeeded": false,
        "effect_verified": false,
        "visual_change_detected": false,
        "input_method": "none",
        "verification_method": "preflight_accessibility",
        "foreground_activated": false,
        "foreground_verification": "not_dispatched",
        "real_cursor_moved": false,
    })
}

#[cfg(target_os = "macos")]
fn record_click_foreground_change(result: &mut Value, verification: Result<bool, String>) {
    // Focus changes are allowed in application mode. This is diagnostic only;
    // neither activation nor a failed probe changes the input/effect result.
    result["foreground_verification"] = json!("window_server_poll_2ms");
    match verification {
        Ok(activated) => {
            result["foreground_activated"] = json!(activated);
        }
        Err(error) => {
            result["foreground_verification"] = json!("unavailable");
            result["foreground_warning"] = json!(error);
            result["foreground_activated"] = Value::Null;
        }
    }
}

#[cfg(target_os = "macos")]
fn execute_background_click(action: &ComputerAction) -> Result<Value, String> {
    let target = background_target(action)?;
    let point = background_local_point(action, target)?;
    let (x, y) = background_point(action, target)?;
    let event_window = mac_background_event_target(target, (x, y));
    let event_target = event_window.target;
    let before = capture_screenshot(action)?;
    let is_single_left_click = action.action == "click"
        && action
            .button
            .as_deref()
            .unwrap_or("left")
            .eq_ignore_ascii_case("left");
    if !event_window.on_screen {
        return Ok(finish_background_click(
            action,
            unsupported_background_click(action, target, event_target, point,
                "The target surface is off-screen; use focus_window and observe again before clicking".to_string()),
            Ok(before),
        ));
    }
    let foreground_monitor = MacForegroundMonitor::start(target.pid);

    // Prefer a semantic action, but custom-drawn controls need mouse events.
    // PID/window routing preserves the target; activation by the app is allowed.
    // Never retry an acknowledged/uncertain action via another input path.
    let dispatch = if is_single_left_click {
        mac_ax_press(event_target, x, y)
    } else {
        MacAxPressOutcome::Unsupported {
            reason: "The requested mouse gesture requires window-targeted mouse events".to_string(),
        }
    };
    let uses_mouse = matches!(dispatch, MacAxPressOutcome::Unsupported { .. });
    if uses_mouse {
        mac_click(
            event_target,
            x,
            y,
            action.button.as_deref(),
            if action.action == "double_click" {
                2
            } else {
                1
            },
        )?;
    }

    thread::sleep(Duration::from_millis(CLICK_SETTLE_MS));
    let mut after = capture_screenshot(action);
    let mut visual_change = after
        .as_ref()
        .ok()
        .and_then(|frame| screenshot_visual_change(&before, frame, (x, y)));
    if !visual_change.map(|change| change.detected).unwrap_or(false) && after.is_ok() {
        thread::sleep(Duration::from_millis(CLICK_SETTLE_MS));
        after = capture_screenshot(action);
        visual_change = after
            .as_ref()
            .ok()
            .and_then(|frame| screenshot_visual_change(&before, frame, (x, y)));
    }
    let changed = visual_change.map(|change| change.detected).unwrap_or(false);
    let mut result = json!({
        "ok": changed,
        "mode": "background_app",
        "action": action.action,
        "summary": if changed {
            "Application click changed the target window"
        } else {
            "Application click could not be verified"
        },
        "cursor": { "x": point.0, "y": point.1 },
        "coordinate_space": "window",
        "window_origin": { "x": target.x, "y": target.y },
        "dispatch_window_id": event_target.window_id.to_string(),
        "dispatch_succeeded": uses_mouse || matches!(dispatch, MacAxPressOutcome::Performed { .. }),
        "effect_verified": changed,
        "visual_change_detected": changed,
        "visual_changed_pixels": visual_change.map(|change| change.changed_pixels),
        "visual_sampled_pixels": visual_change.map(|change| change.sampled_pixels),
        "input_method": if uses_mouse { "quartz_event" } else { "accessibility_action" },
        "verification_method": "screenshot_difference",
        "real_cursor_moved": false,
    });
    match dispatch {
        MacAxPressOutcome::Performed { action: ax_action } => {
            result["accessibility_action"] = json!(ax_action);
        }
        MacAxPressOutcome::Uncertain { reason } => {
            result["verification_warning"] = json!(reason);
        }
        MacAxPressOutcome::Unsupported { reason } => {
            result["fallback_reason"] = json!(reason);
        }
    }
    if !changed {
        result["error_code"] = json!("background_click_unverified");
        result["error"] = json!(
            "The click may have arrived but no target-area change was verified; observe instead of repeating the click"
        );
    }
    let mut result = finish_background_click(action, result, after);
    record_click_foreground_change(
        &mut result,
        foreground_monitor.and_then(|monitor| monitor.finish()),
    );
    Ok(result)
}

#[cfg(target_os = "macos")]
fn execute_background(request: ExecuteRequest) -> Result<Value, String> {
    let action = &request.action;
    if matches!(
        action.action.as_str(),
        "move"
            | "click"
            | "double_click"
            | "drag"
            | "scroll"
            | "type"
            | "keypress"
            | "focus_window"
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
        "list_displays" => return Err(
            "background_app mode does not expose the full desktop or displays; use list_windows"
                .to_string(),
        ),
        "move" => {
            let target = background_target(action)?;
            let local = background_local_point(action, target)?;
            let (x, y) = background_point(action, target)?;
            let event_target = mac_background_event_target(target, (x, y)).target;
            mac_post_mouse(
                event_target,
                CGEventType::MouseMoved,
                CGMouseButton::Left,
                x,
                y,
                0,
            )?;
            cursor = json!({ "x": local.0, "y": local.1 });
            "Moved application pointer".to_string()
        }
        "click" | "double_click" => {
            return execute_background_click(action);
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
            let end_local = background_local_point(&end_action, target)?;
            let end = background_point(&end_action, target)?;
            let event_target = mac_background_event_target(target, start).target;
            mac_drag(
                event_target,
                start,
                end,
                action.button.as_deref(),
                action.duration_ms.unwrap_or(400).min(30_000),
            )?;
            cursor = json!({ "x": end_local.0, "y": end_local.1 });
            "Dragged in application window".to_string()
        }
        "scroll" => {
            let (delta_x, delta_y) = scroll_delta(action)?;
            let target = background_target(action)?;
            let local = background_local_point(action, target)?;
            let point = background_point(action, target)?;
            let event_target = mac_background_event_target(target, point).target;
            mac_scroll(Some(event_target), point, delta_x, delta_y)?;
            cursor = json!({ "x": local.0, "y": local.1 });
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
                return Err(format!(
                    "Unable to open app in background; process exited with {status}"
                ));
            }
            format!("Opened {name} in background")
        }
        "focus_window" => {
            let target = background_target(action)?;
            let name = Window::all()
                .map_err(|error| error.to_string())?
                .iter()
                .find(|window| window.id().ok() == Some(target.window_id))
                .ok_or_else(|| "Target window is no longer available".to_string())?
                .app_name()
                .map_err(|error| error.to_string())?;
            focus_app(&name)?;
            format!("Focused {name}; observe the target window before further input")
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
        "coordinate_space": "window",
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
            Ok(frame) => {
                if frame["background_observation_limited"] == Value::Bool(true) {
                    result["background_observation_limited"] = Value::Bool(true);
                    result["observation_warning"] = frame["observation_warning"].clone();
                    if action.action == "observe" {
                        result["summary"] = Value::String(
                            "Observed an application window group; a hidden auxiliary window may have incomplete pixels"
                                .to_string(),
                        );
                    }
                }
                result["screenshot"] = frame;
            }
            Err(error) if action.action == "observe" => return Err(error),
            Err(error) => result["screenshot_error"] = Value::String(error),
        }
    }
    Ok(result)
}

#[cfg(not(target_os = "macos"))]
fn execute_background(_request: ExecuteRequest) -> Result<Value, String> {
    Err("background_app mode is unavailable on this platform".to_string())
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

    #[cfg(target_os = "macos")]
    struct MacInputTestHost {
        launch: std::process::Child,
        state_path: std::path::PathBuf,
        _directory: tempfile::TempDir,
    }

    #[cfg(target_os = "macos")]
    impl MacInputTestHost {
        fn start() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let bundle = directory.path().join("InputFixture.app");
            let binaries = bundle.join("Contents/MacOS");
            std::fs::create_dir_all(&binaries).unwrap();
            std::fs::write(
                bundle.join("Contents/Info.plist"),
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>input-fixture</string>
<key>CFBundleIdentifier</key><string>io.crabcode.input-test</string>
<key>CFBundleName</key><string>CrabCode input fixture</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#,
            )
            .unwrap();
            let status = Command::new("xcrun")
                .args(["swiftc", "tests/fixtures/scroll_host.swift", "-o"])
                .arg(binaries.join("input-fixture"))
                .status()
                .unwrap();
            assert!(status.success());
            let state_path = directory.path().join("state.json");
            // A bare AppKit executable activates asynchronously at launch.
            // Use LaunchServices' background flag so activation assertions
            // measure the input path, not the fixture's startup sequence.
            let launch = Command::new("open")
                .args(["-g", "-n", "-W"])
                .arg(&bundle)
                .arg("--args")
                .arg(&state_path)
                .spawn()
                .unwrap();
            Self {
                launch,
                state_path,
                _directory: directory,
            }
        }

        fn state(&self) -> Option<Value> {
            serde_json::from_slice(&std::fs::read(&self.state_path).ok()?).ok()
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for MacInputTestHost {
        fn drop(&mut self) {
            if let Some(pid) = self.state().and_then(|state| state["pid"].as_i64()) {
                // This PID is published by our isolated, temporary fixture.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            }
            let _ = self.launch.kill();
            let _ = self.launch.wait();
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn background_click_focus_diagnostics_preserve_action_result() {
        for verification in [
            Ok(false),
            Ok(true),
            Err("foreground probe failed".to_string()),
        ] {
            for succeeded in [true, false] {
                let mut result = json!({
                    "ok": succeeded,
                    "effect_verified": succeeded,
                    "visual_change_detected": succeeded,
                    "dispatch_succeeded": true,
                });
                if !succeeded {
                    result["error_code"] = json!("background_click_unverified");
                }
                record_click_foreground_change(&mut result, verification.clone());
                assert_eq!(result["ok"], succeeded);
                assert_eq!(result["effect_verified"], succeeded);
                assert_eq!(result["visual_change_detected"], succeeded);
                assert_eq!(result["dispatch_succeeded"], true);
                if succeeded {
                    assert!(result["error_code"].is_null());
                } else {
                    assert_eq!(result["error_code"], "background_click_unverified");
                }
                match &verification {
                    Ok(activated) => assert_eq!(result["foreground_activated"], *activated),
                    Err(error) => {
                        assert!(result["foreground_activated"].is_null());
                        assert_eq!(result["foreground_warning"], *error);
                    }
                }
            }
        }
    }

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

    #[test]
    fn background_coordinates_are_window_local_and_translate_to_desktop_space() {
        let target = WindowTarget {
            window_id: 107,
            pid: 1084,
            x: 0,
            y: 33,
            width: 1492,
            height: 868,
        };
        let action: ComputerAction = serde_json::from_value(json!({
            "action": "click",
            "window_id": "107",
            "x": 212,
            "y": 810,
        }))
        .unwrap();
        assert_eq!(background_local_point(&action, target).unwrap(), (212, 810));
        assert_eq!(background_point(&action, target).unwrap(), (212, 843));

        for (x, y) in [(-1, 0), (0, -1), (1492, 0), (0, 868)] {
            let action: ComputerAction = serde_json::from_value(json!({
                "action": "click",
                "window_id": "107",
                "x": x,
                "y": y,
            }))
            .unwrap();
            assert!(background_point(&action, target).is_err());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn background_window_group_requires_ax_confirmation_for_hidden_dialogs() {
        let root = WindowTarget {
            window_id: 107,
            pid: 1084,
            x: 0,
            y: 33,
            width: 1492,
            height: 868,
        };
        let info = |window_id, x, y, width, height, title: &str, on_screen| MacWindowInfo {
            target: WindowTarget {
                window_id,
                pid: 1084,
                x,
                y,
                width,
                height,
            },
            title: title.to_string(),
            layer: 0,
            on_screen,
        };
        let windows = vec![
            info(10057, 0, 33, 1492, 868, "", false),
            info(8676, 0, 33, 1492, 868, "", true),
            info(200, 400, 200, 500, 500, "", true),
            info(153, 0, 482, 64, 64, "", false),
            info(107, 0, 33, 1492, 868, "昆仑万维", true),
        ];
        let current_ax_windows = HashSet::from([107, 8676]);
        let group = mac_window_group_from_info(root, &windows, &current_ax_windows);
        assert_eq!(
            group
                .components
                .iter()
                .map(|window| window.target.window_id)
                .collect::<Vec<_>>(),
            vec![200, 107]
        );
        let event_target = mac_event_target_from_group(group, root, (450, 250));
        assert_eq!(event_target.target.window_id, 200);
        assert!(event_target.on_screen);

        let current_ax_windows = HashSet::from([107, 8676, 10057]);
        let group = mac_window_group_from_info(root, &windows, &current_ax_windows);
        assert_eq!(
            group
                .components
                .iter()
                .map(|window| window.target.window_id)
                .collect::<Vec<_>>(),
            vec![10057, 200, 107]
        );
        let event_target = mac_event_target_from_group(group, root, (450, 250));
        assert_eq!(event_target.target.window_id, 10057);
        assert!(!event_target.on_screen);

        let group = mac_window_group_from_info(root, &windows, &HashSet::new());
        assert_eq!(
            group
                .components
                .iter()
                .map(|window| window.target.window_id)
                .collect::<Vec<_>>(),
            vec![200, 107]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn click_verification_ignores_unrelated_window_changes() {
        use xcap::image::Rgba;

        let base = RgbaImage::from_pixel(256, 256, Rgba([0, 0, 0, 255]));
        let capture = |image| ScreenshotCapture {
            image,
            origin_x: 0,
            origin_y: 30,
            target: "window:42".to_string(),
            component_window_ids: Vec::new(),
            hidden_component_window_ids: Vec::new(),
            component_capture_errors: Vec::new(),
        };
        let before = capture(base.clone());
        let same = capture(base.clone());
        assert!(
            !screenshot_visual_change(&before, &same, (128, 158))
                .unwrap()
                .detected
        );

        let mut unrelated_pixels = base.clone();
        for y in 0..12 {
            for x in 0..12 {
                unrelated_pixels.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        let unrelated = capture(unrelated_pixels);
        assert!(
            !screenshot_visual_change(&before, &unrelated, (128, 158))
                .unwrap()
                .detected
        );

        let mut local_pixels = base;
        for y in 125..131 {
            for x in 125..131 {
                local_pixels.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        let local = capture(local_pixels);
        let change = screenshot_visual_change(&before, &local, (128, 158)).unwrap();
        assert!(change.detected);
        assert_eq!(change.changed_pixels, 36);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "read-only live window-group capture; set CRABCODE_TEST_WINDOW_ID explicitly"]
    fn macos_background_window_group_capture_is_read_only() {
        let window_id = std::env::var("CRABCODE_TEST_WINDOW_ID")
            .expect("CRABCODE_TEST_WINDOW_ID must name a live window");
        let target = window_target(&window_id).expect("live root window was not found");
        let process_windows = mac_all_window_info()
            .expect("window enumeration failed")
            .into_iter()
            .filter(|window| window.target.pid == target.pid)
            .collect::<Vec<_>>();
        eprintln!("process_windows={process_windows:?}");
        eprintln!(
            "accessibility_window_ids={:?}",
            mac_ax_application_window_ids(target.pid)
        );
        let capture = mac_capture_window_group(target).expect("window group capture failed");
        assert_eq!(capture.image.dimensions(), (target.width, target.height));
        assert_eq!(capture.origin_x, target.x);
        assert_eq!(capture.origin_y, target.y);
        assert!(capture.component_window_ids.contains(&target.window_id));
        if let Ok(expected) = std::env::var("CRABCODE_TEST_EXPECT_COMPONENT_ID") {
            let expected = expected
                .parse::<u32>()
                .expect("component id was not numeric");
            assert!(
                capture.component_window_ids.contains(&expected),
                "component {expected} missing from {:?}",
                capture.component_window_ids
            );
        }
        if let Ok(path) = std::env::var("CRABCODE_TEST_CAPTURE_OUTPUT") {
            capture
                .image
                .save(path)
                .expect("unable to save read-only capture fixture output");
        }
        eprintln!(
            "components={:?} hidden={:?} errors={:?}",
            capture.component_window_ids,
            capture.hidden_component_window_ids,
            capture.component_capture_errors
        );
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
    fn macos_background_pointer_targets_one_of_two_windows_without_focus() {
        mac_require_input_permission()
            .expect("grant Accessibility to the test runner before running this ignored test");
        let host = MacInputTestHost::start();
        let read_state = || host.state();
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
        let assert_desktop_preserved = |after: &Value| {
            assert_eq!(
                after["active"], false,
                "target state was not restored: {after}"
            );
            assert_eq!(
                after["ever_frontmost"], false,
                "target stole foreground: {after}"
            );
            assert_eq!(
                after["ever_raised"], false,
                "target window was raised: {after}"
            );
            // Keep strict cursor/focus checks for idle desktop runs, while also
            // allowing this regression to run alongside real user activity.
            if std::env::var_os("CRABCODE_TEST_ALLOW_USER_INPUT").is_none() {
                assert_eq!(after["cursor"], initial["cursor"]);
                assert_eq!(after["frontmost_pid"], initial["frontmost_pid"]);
            }
        };
        assert_desktop_preserved(&after);

        let foreground_monitor = MacForegroundMonitor::start(target.pid).unwrap();
        mac_click(target, point.0, point.1, Some("left"), 1).unwrap();
        thread::sleep(Duration::from_millis(500));
        assert!(!foreground_monitor.finish().unwrap());
        let after = read_state().unwrap();
        assert_eq!(
            after["target_clicks"], 1,
            "click did not reach target: {after}"
        );
        assert_eq!(after["decoy_clicks"], 0);
        assert_eq!(after["modifiers"], json!([0]));
        assert_desktop_preserved(&after);

        mac_click(target, point.0, point.1, Some("left"), 2).unwrap();
        mac_click(target, point.0, point.1, Some("right"), 1).unwrap();
        mac_click(target, point.0, point.1, Some("middle"), 1).unwrap();
        mac_drag(
            target,
            point,
            (point.0 + 40, point.1 + 20),
            Some("left"),
            100,
        )
        .unwrap();
        thread::sleep(Duration::from_millis(500));
        let after = read_state().unwrap();
        assert_eq!(after["target_clicks"], 4, "missing clicks: {after}");
        assert_eq!(after["click_counts"], json!([1, 1, 2, 1]));
        let click_times = after["click_times"].as_array().unwrap();
        assert!(click_times[2].as_f64().unwrap() - click_times[1].as_f64().unwrap() >= 0.08);
        assert_eq!(after["target_other_clicks"], 2);
        assert!(after["target_drags"].as_u64().unwrap() > 0);
        assert_eq!(after["modifiers"], json!([0, 0, 0, 0]));
        for field in ["decoy_clicks", "decoy_other_clicks", "decoy_drags"] {
            assert_eq!(after[field], 0, "input reached decoy: {after}");
        }
        assert_desktop_preserved(&after);

        let button_point = (
            initial["button_x"].as_i64().unwrap() as i32,
            initial["button_y"].as_i64().unwrap() as i32,
        );
        mac_click(target, button_point.0, button_point.1, Some("left"), 1).unwrap();
        thread::sleep(Duration::from_millis(500));
        let after = read_state().unwrap();
        assert_eq!(
            after["button_presses"], 1,
            "native button did not activate: {after}"
        );
        assert_desktop_preserved(&after);

        let decoy_target = WindowTarget {
            window_id: initial["decoy_id"].as_u64().unwrap() as u32,
            ..target
        };
        let decoy_button_point = (
            initial["decoy_button_x"].as_i64().unwrap() as i32,
            initial["decoy_button_y"].as_i64().unwrap() as i32,
        );
        match mac_ax_press(decoy_target, decoy_button_point.0, decoy_button_point.1) {
            MacAxPressOutcome::Performed { action } => assert_eq!(action, "AXPress"),
            outcome => panic!("AXPress did not activate the native button: {outcome:?}"),
        }
        thread::sleep(Duration::from_millis(500));
        let after = read_state().unwrap();
        assert_eq!(
            after["decoy_button_presses"], 1,
            "AXPress did not activate native button: {after}"
        );
        assert_desktop_preserved(&after);

        // The production path must not emit any application-activation record.
        thread::sleep(Duration::from_millis(200));
        assert_desktop_preserved(&read_state().unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires macOS Accessibility and Screen Recording; launches isolated click test windows"]
    fn macos_background_click_allows_activation_and_mouse_fallback() {
        mac_require_input_permission()
            .expect("grant Accessibility to the test runner before running this ignored test");
        let host = MacInputTestHost::start();
        let read_state = || host.state();
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

        let assert_desktop_preserved = |after: &Value| {
            assert_eq!(
                after["active"], false,
                "target state was not restored: {after}"
            );
            assert_eq!(
                after["ever_frontmost"], false,
                "target stole foreground: {after}"
            );
            assert_eq!(
                after["ever_raised"], false,
                "target window was raised: {after}"
            );
            // Keep strict cursor/focus checks for idle desktop runs, while also
            // allowing this regression to run alongside real user activity.
            if std::env::var_os("CRABCODE_TEST_ALLOW_USER_INPUT").is_none() {
                assert_eq!(after["cursor"], initial["cursor"]);
                assert_eq!(after["frontmost_pid"], initial["frontmost_pid"]);
            }
        };

        let button_point = (
            initial["button_x"].as_i64().unwrap() as i32,
            initial["button_y"].as_i64().unwrap() as i32,
        );
        let decoy_target = WindowTarget {
            window_id: initial["decoy_id"].as_u64().unwrap() as u32,
            ..target
        };
        let decoy_button_point = (
            initial["decoy_button_x"].as_i64().unwrap() as i32,
            initial["decoy_button_y"].as_i64().unwrap() as i32,
        );
        // The app-wide AX hit test sees the overlapping decoy. Resolve the
        // covered target's own AX button instead of falling back to a raw click.
        let foreground_monitor = MacForegroundMonitor::start(target.pid).unwrap();
        match mac_ax_press(target, button_point.0, button_point.1) {
            MacAxPressOutcome::Performed { action } => assert_eq!(action, "AXPress"),
            outcome => panic!("window-scoped AXPress did not find the covered button: {outcome:?}"),
        }
        thread::sleep(Duration::from_millis(500));
        assert!(!foreground_monitor.finish().unwrap());
        let after = read_state().unwrap();
        assert_eq!(after["button_presses"], 1);
        assert_eq!(after["decoy_button_presses"], 0);
        assert_desktop_preserved(&after);

        // Exercise the actual ComputerUse entry point, including observation,
        // semantic dispatch and focus diagnostics.
        let click = json!({
            "action": "click",
            "window_id": decoy_target.window_id.to_string(),
            "x": decoy_button_point.0 - target.x,
            "y": decoy_button_point.1 - target.y,
            "include_screenshot": false,
        });
        let result = execute(
            serde_json::from_value(json!({
                "mode": "background_app", "action": click,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["input_method"], "accessibility_action", "{result}");
        let after_click = read_state().unwrap();
        assert_eq!(
            after_click["decoy_button_presses"], 1,
            "{result}; {after_click}"
        );
        // An occluded AppKit window can retain stale screenshot pixels. The
        // fixture's action counter independently verifies delivery; the host
        // must still report unverified if those pixels have not changed.
        assert_eq!(result["dispatch_succeeded"], true, "{result}");
        if result["visual_change_detected"] == false {
            assert_eq!(result["ok"], false, "{result}");
            assert_eq!(
                result["error_code"], "background_click_unverified",
                "{result}"
            );
        }
        assert_eq!(result["foreground_activated"], false, "{result}");
        assert_desktop_preserved(&after_click);

        // AXPress may legitimately activate the app. The result must preserve
        // the observed effect instead of reporting a foreground violation.
        let mut activating_click = click.clone();
        activating_click["x"] =
            json!(initial["activating_button_x"].as_i64().unwrap() - i64::from(target.x));
        activating_click["y"] =
            json!(initial["activating_button_y"].as_i64().unwrap() - i64::from(target.y));
        let result = execute(
            serde_json::from_value(json!({
                "mode": "background_app", "action": activating_click,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["input_method"], "accessibility_action", "{result}");
        assert_eq!(result["dispatch_succeeded"], true, "{result}");
        let after_activation = read_state().unwrap();
        assert_eq!(
            result["foreground_activated"], true,
            "{result}; {after_activation}"
        );
        assert_eq!(result["ok"], result["effect_verified"], "{result}");
        assert!(
            result["error_code"].is_null() || result["error_code"] == "background_click_unverified",
            "{result}"
        );
        assert_eq!(
            after_activation["activating_button_presses"], 1,
            "{after_activation}"
        );
        assert_eq!(
            after_activation["ever_frontmost"], true,
            "{after_activation}"
        );

        let mut custom_point = click.clone();
        custom_point["x"] = json!(point.0 - target.x);
        custom_point["y"] = json!(point.1 - target.y);
        let mut double_click = custom_point.clone();
        double_click["action"] = json!("double_click");
        let mut right_click = custom_point.clone();
        right_click["button"] = json!("right");
        let mut middle_click = custom_point.clone();
        middle_click["button"] = json!("middle");
        for (action, clicks, other_clicks) in [
            (custom_point, 1, 0),
            (double_click, 3, 0),
            (right_click, 3, 1),
            (middle_click, 3, 2),
        ] {
            let result = execute(
                serde_json::from_value(json!({
                    "mode": "background_app", "action": action,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(result["dispatch_succeeded"], true, "{result}");
            assert_eq!(result["input_method"], "quartz_event", "{result}");
            let after = read_state().unwrap();
            assert_eq!(after["decoy_clicks"], clicks, "{result}; {after}");
            assert_eq!(
                after["decoy_other_clicks"], other_clicks,
                "{result}; {after}"
            );
            assert_eq!(after["target_clicks"], 0, "{after}");
            assert_eq!(after["target_other_clicks"], 0, "{after}");
        }
        let focus = execute(
            serde_json::from_value(json!({
                "mode": "background_app", "action": {
                    "action": "focus_window", "window_id": decoy_target.window_id.to_string(),
                    "include_screenshot": false,
                },
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(focus["ok"], true, "{focus}");
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
