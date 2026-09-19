use base64::{engine::general_purpose::STANDARD, Engine as _};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
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

const MAX_SCREENSHOT_BYTES: usize = 20 * 1024 * 1024;

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
    platform: &'static str,
    displays: Vec<DisplayInfo>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExecuteRequest {
    action: ComputerAction,
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

fn detect_capabilities() -> ComputerUseCapabilities {
    let found = match monitors() {
        Ok(found) if !found.is_empty() => found,
        Ok(_) => {
            return ComputerUseCapabilities {
                gui_available: false,
                platform: std::env::consts::OS,
                displays: Vec::new(),
                reason: Some("No graphical displays were detected".to_string()),
            }
        }
        Err(error) => {
            return ComputerUseCapabilities {
                gui_available: false,
                platform: std::env::consts::OS,
                displays: Vec::new(),
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
    let capture_error = capture_target.and_then(|(monitor, _)| monitor.capture_image().err());
    let input_error = Enigo::new(&Settings::default()).err();
    let reason = capture_error
        .map(|error| format!("Screen capture is unavailable: {error}"))
        .or_else(|| input_error.map(|error| format!("Desktop input is unavailable: {error}")));
    ComputerUseCapabilities {
        gui_available: reason.is_none(),
        platform: std::env::consts::OS,
        displays,
        reason,
    }
}

#[tauri::command]
pub async fn computer_use_capabilities() -> ComputerUseCapabilities {
    tauri::async_runtime::spawn_blocking(detect_capabilities)
        .await
        .unwrap_or_else(|error| ComputerUseCapabilities {
            gui_available: false,
            platform: std::env::consts::OS,
            displays: Vec::new(),
            reason: Some(format!("Computer Use capability detection failed: {error}")),
        })
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
            if let (Some(x), Some(y)) = (action.x, action.y) {
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|error| error.to_string())?;
            }
            if let Some(delta_x) = action.delta_x.filter(|value| *value != 0) {
                enigo
                    .scroll(delta_x, Axis::Horizontal)
                    .map_err(|error| error.to_string())?;
            }
            if let Some(delta_y) = action.delta_y.filter(|value| *value != 0) {
                enigo
                    .scroll(delta_y, Axis::Vertical)
                    .map_err(|error| error.to_string())?;
            }
            Ok("Scrolled".to_string())
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

fn execute(request: ExecuteRequest) -> Result<Value, String> {
    let mut enigo = Enigo::new(&Settings::default()).map_err(|error| error.to_string())?;
    let summary = perform_action(&request.action, &mut enigo)?;
    let cursor = enigo
        .location()
        .map(|(x, y)| json!({ "x": x, "y": y }))
        .unwrap_or(Value::Null);

    let mut result = json!({
        "ok": true,
        "action": request.action.action,
        "summary": summary,
        "cursor": cursor,
    });
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
        match screenshot(&request.action) {
            Ok(frame) => result["screenshot"] = frame,
            Err(error) if request.action.action == "observe" => return Err(error),
            Err(error) => result["screenshot_error"] = Value::String(error),
        }
    }
    Ok(result)
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
}
