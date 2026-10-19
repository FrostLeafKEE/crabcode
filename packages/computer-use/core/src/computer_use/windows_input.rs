//! Windows desktop input that does not go through Unicode keystroke injection.
//!
//! A burst of KEYEVENTF_UNICODE events is queued immediately and then drained
//! slowly by the foreground app (often the IME), so a screenshot taken right
//! after `type` shows only the first characters. Controls such as Explorer's
//! rename field and the Edge address bar also ignore Ctrl+Unicode-V; they
//! require a virtual-key chord. Text is therefore pasted, and keypress sends
//! virtual keys. The pointer uses SetCursorPos so multi-monitor coordinates
//! match the screenshot, including displays left of the primary.

use std::thread;
use std::time::Duration;

use super::windows_clipboard::with_clipboard_text;
use serde_json::{json, Value};
use windows::core::{w, HSTRING};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, MapVirtualKeyW, SendInput, VkKeyScanExW, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC,
    VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_LWIN, VK_MENU,
    VK_NEXT, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
    VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, SetCursorPos, SW_SHOWNORMAL,
};

const PASTE_SETTLE_MS: u64 = 200;

#[derive(Debug, PartialEq, Eq)]
enum TypePlan {
    Empty,
    Enter,
    Insert { text: String, enter_after: bool },
}

#[derive(Clone, Copy)]
struct Stroke {
    vk: VIRTUAL_KEY,
    extended: bool,
}

pub(super) fn move_cursor(x: i32, y: i32) -> Result<(), String> {
    unsafe { SetCursorPos(x, y) }
        .map_err(|error| format!("Unable to move pointer to ({x}, {y}): {error}"))
}

pub(super) fn type_text(text: &str) -> Result<(), String> {
    match plan_type(text) {
        TypePlan::Empty => Ok(()),
        TypePlan::Enter => press_keys(&["ENTER".to_string()]),
        TypePlan::Insert { text, enter_after } => {
            with_clipboard_text(&text, || {
                press_keys(&["CTRL".to_string(), "v".to_string()])?;
                // The target reads the clipboard on its own thread. Restoring
                // earlier can make Explorer or Edge paste the previous contents.
                thread::sleep(Duration::from_millis(PASTE_SETTLE_MS));
                Ok(())
            })?;
            if enter_after {
                press_keys(&["ENTER".to_string()])?;
            }
            Ok(())
        }
    }
}

pub(super) fn press_keys(names: &[String]) -> Result<(), String> {
    let sequence = key_sequence(names)?;
    let mut input = Vec::with_capacity(sequence.len() * 2);
    for stroke in &sequence {
        input.push(key_event(*stroke, false));
    }
    for stroke in sequence.iter().rev() {
        input.push(key_event(*stroke, true));
    }
    send_input(&input)
}

fn key_sequence(names: &[String]) -> Result<Vec<Stroke>, String> {
    if names.is_empty() {
        return Err("keys cannot be empty".to_string());
    }
    let mut sequence = Vec::new();
    let mut shift_held = false;
    let shortcut = names.iter().any(|name| {
        named_vk(&name.trim().to_ascii_uppercase())
            .is_some_and(|vk| matches!(vk, VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN))
    });
    for name in names {
        // Shortcut letters name physical keys, not shifted text characters.
        // Explicit SHIFT and shifted punctuation still retain their meaning.
        let key = if shortcut && name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() {
            name.to_ascii_lowercase()
        } else {
            name.clone()
        };
        for stroke in strokes_for_name(&key)? {
            if stroke.vk == VK_SHIFT || stroke.vk == VK_RSHIFT {
                if shift_held {
                    continue;
                }
                shift_held = true;
            }
            sequence.push(stroke);
        }
    }
    Ok(sequence)
}

pub(super) fn open_app(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.contains('\0') {
        return Err("An application name or path without NUL characters is required".into());
    }
    // Pass the application as one literal shell target. PowerShell -Command
    // appends trailing arguments to its source instead of binding $args.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(name),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("Unable to open app: Windows shell error {code}"))
    }
}

pub(super) fn foreground_window() -> Option<Value> {
    let window = unsafe { GetForegroundWindow() };
    if window.0.is_null() {
        return None;
    }
    let mut buffer = [0u16; 512];
    let length = unsafe { GetWindowTextW(window, &mut buffer) };
    let title = String::from_utf16_lossy(&buffer[..length as usize]);
    Some(json!({
        "id": (window.0 as usize as u32).to_string(),
        "title": title,
    }))
}

fn plan_type(text: &str) -> TypePlan {
    if text.is_empty() {
        return TypePlan::Empty;
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let without_one_trailing = normalized.strip_suffix('\n').unwrap_or(&normalized);
    let submitted = without_one_trailing.len() != normalized.len();
    if !without_one_trailing.contains('\n') {
        if without_one_trailing.is_empty() {
            return TypePlan::Enter;
        }
        return TypePlan::Insert {
            text: without_one_trailing.to_string(),
            enter_after: submitted,
        };
    }
    TypePlan::Insert {
        text: normalized.replace('\n', "\r\n"),
        enter_after: false,
    }
}

fn strokes_for_name(name: &str) -> Result<Vec<Stroke>, String> {
    let upper = name.trim().to_ascii_uppercase();
    if let Some(vk) = named_vk(upper.as_str()) {
        return Ok(vec![Stroke {
            vk,
            extended: extended_key(vk),
        }]);
    }
    let mut chars = name.chars();
    let character = chars
        .next()
        .ok_or_else(|| "Key cannot be empty".to_string())?;
    if chars.next().is_some() || !character.is_ascii() {
        return Err(format!("Unknown key: {name}"));
    }
    let window = unsafe { GetForegroundWindow() };
    let thread = unsafe { GetWindowThreadProcessId(window, None) };
    let layout = unsafe { GetKeyboardLayout(thread) };
    let mapped = unsafe { VkKeyScanExW(character as u16, layout) };
    if mapped < 0 {
        return Err(format!("Unknown key: {name}"));
    }
    let vk = VIRTUAL_KEY((mapped & 0xFF) as u16);
    let mut shift_state = (mapped >> 8) as u8;
    let mut strokes = Vec::new();
    if shift_state & 1 != 0 {
        strokes.push(Stroke {
            vk: VK_SHIFT,
            extended: false,
        });
        shift_state &= !1;
    }
    if shift_state != 0 {
        return Err(format!("Unknown key: {name}"));
    }
    strokes.push(Stroke {
        vk,
        extended: extended_key(vk),
    });
    Ok(strokes)
}

fn named_vk(name: &str) -> Option<VIRTUAL_KEY> {
    Some(match name {
        "ALT" | "OPTION" => VK_MENU,
        "BACKSPACE" => VIRTUAL_KEY(0x08),
        "CAPSLOCK" | "CAPS_LOCK" => VIRTUAL_KEY(0x14),
        "CMD" | "COMMAND" | "META" | "WIN" | "WINDOWS" => VK_LWIN,
        "CTRL" | "CONTROL" => VK_CONTROL,
        "DELETE" | "DEL" => VK_DELETE,
        "DOWN" | "ARROWDOWN" => VK_DOWN,
        "END" => VK_END,
        "ENTER" | "RETURN" => VK_RETURN,
        "ESC" | "ESCAPE" => VIRTUAL_KEY(0x1B),
        "HOME" => VK_HOME,
        "LEFT" | "ARROWLEFT" => VK_LEFT,
        "PAGEDOWN" | "PAGE_DOWN" => VK_NEXT,
        "PAGEUP" | "PAGE_UP" => VK_PRIOR,
        "RIGHT" | "ARROWRIGHT" => VK_RIGHT,
        "SHIFT" => VK_SHIFT,
        "SPACE" => VIRTUAL_KEY(0x20),
        "TAB" => VIRTUAL_KEY(0x09),
        "UP" | "ARROWUP" => VK_UP,
        "F1" => VIRTUAL_KEY(0x70),
        "F2" => VIRTUAL_KEY(0x71),
        "F3" => VIRTUAL_KEY(0x72),
        "F4" => VIRTUAL_KEY(0x73),
        "F5" => VIRTUAL_KEY(0x74),
        "F6" => VIRTUAL_KEY(0x75),
        "F7" => VIRTUAL_KEY(0x76),
        "F8" => VIRTUAL_KEY(0x77),
        "F9" => VIRTUAL_KEY(0x78),
        "F10" => VIRTUAL_KEY(0x79),
        "F11" => VIRTUAL_KEY(0x7A),
        "F12" => VIRTUAL_KEY(0x7B),
        _ => return None,
    })
}

fn extended_key(vk: VIRTUAL_KEY) -> bool {
    matches!(
        vk,
        VK_RMENU
            | VK_RCONTROL
            | VK_LEFT
            | VK_UP
            | VK_RIGHT
            | VK_DOWN
            | VK_PRIOR
            | VK_NEXT
            | VK_END
            | VK_HOME
            | VK_DELETE
            | VK_LWIN
            | VK_RWIN
    )
}

fn key_event(stroke: Stroke, release: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS::default();
    if stroke.extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if release {
        flags |= KEYEVENTF_KEYUP;
    }
    let scan = unsafe { MapVirtualKeyW(u32::from(stroke.vk.0), MAPVK_VK_TO_VSC) } as u16;
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: stroke.vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_input(input: &[INPUT]) -> Result<(), String> {
    if input.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(input, std::mem::size_of::<INPUT>() as i32) };
    if sent == input.len() as u32 {
        Ok(())
    } else {
        Err("Not all keyboard events were sent. They may have been blocked by UIPI".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_newline_submits_after_paste() {
        assert_eq!(plan_type(""), TypePlan::Empty);
        assert_eq!(plan_type("\n"), TypePlan::Enter);
        assert_eq!(
            plan_type("calc"),
            TypePlan::Insert {
                text: "calc".to_string(),
                enter_after: false,
            }
        );
        assert_eq!(
            plan_type("calc\n"),
            TypePlan::Insert {
                text: "calc".to_string(),
                enter_after: true,
            }
        );
        assert_eq!(
            plan_type("a\nb\n"),
            TypePlan::Insert {
                text: "a\r\nb\r\n".to_string(),
                enter_after: false,
            }
        );
        assert_eq!(
            plan_type("a\n\n"),
            TypePlan::Insert {
                text: "a\r\n\r\n".to_string(),
                enter_after: false,
            }
        );
    }

    #[test]
    fn ctrl_v_is_a_virtual_key_chord_without_shift() {
        let strokes = strokes_for_name("CTRL")
            .unwrap()
            .into_iter()
            .chain(strokes_for_name("v").unwrap())
            .map(|stroke| stroke.vk.0)
            .collect::<Vec<_>>();
        assert_eq!(strokes, vec![VK_CONTROL.0, b'V' as u16]);
    }

    #[test]
    fn uppercase_letter_requests_shift() {
        let strokes = strokes_for_name("A")
            .unwrap()
            .into_iter()
            .map(|stroke| stroke.vk.0)
            .collect::<Vec<_>>();
        assert_eq!(strokes, vec![VK_SHIFT.0, b'A' as u16]);
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop with a foreground window"]
    fn foreground_window_reports_an_id_and_title() {
        let window = foreground_window().expect("a window is in the foreground");
        assert!(!window["id"].as_str().unwrap_or("").is_empty());
        assert!(window["title"].is_string());
    }

    #[test]
    fn shortcut_letters_do_not_implicitly_hold_shift() {
        for letter in ["A", "C", "L", "S", "V", "a", "c", "l", "s", "v"] {
            let keys = key_sequence(&["CTRL".into(), letter.into()]).unwrap();
            assert_eq!(
                keys.iter().map(|stroke| stroke.vk.0).collect::<Vec<_>>(),
                vec![
                    VK_CONTROL.0,
                    letter.to_ascii_uppercase().as_bytes()[0] as u16
                ]
            );
        }
        let keys = key_sequence(&["CTRL".into(), "SHIFT".into(), "L".into()]).unwrap();
        assert_eq!(
            keys.iter().map(|stroke| stroke.vk.0).collect::<Vec<_>>(),
            vec![VK_CONTROL.0, VK_SHIFT.0, b'L' as u16]
        );
        assert!(key_sequence(&[]).is_err());
    }

    #[test]
    fn open_app_rejects_invalid_targets_before_launch() {
        assert!(open_app("").is_err());
        assert!(open_app("  ").is_err());
        assert!(open_app("calc\0.exe").is_err());
    }

    #[test]
    fn open_app_launches_an_executable_with_a_literal_path() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("launched");
        let source = dir.path().join("fixture.rs");
        let executable = dir.path().join("app with spaces ' 中文.exe");
        std::fs::write(&source, format!(
            "#![windows_subsystem = \"windows\"]\nfn main() {{ std::fs::write({:?}, b\"launched\").unwrap(); }}", marker
        )).unwrap();
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("rustc")
            .arg("--crate-name")
            .arg("launch_fixture")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        open_app(executable.to_str().unwrap()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(std::fs::read(&marker).unwrap(), b"launched");
    }
}
