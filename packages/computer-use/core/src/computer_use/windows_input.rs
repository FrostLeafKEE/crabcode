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

use serde_json::{json, Value};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, MapVirtualKeyW, SendInput, VkKeyScanExW, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC,
    VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_LWIN, VK_MENU,
    VK_NEXT, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, SetCursorPos,
};

const PASTE_SETTLE_MS: u64 = 200;
const MAX_PASTE_BYTES: usize = 1024 * 1024;
const CF_UNICODETEXT: u32 = 13;

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
    if names.is_empty() {
        return Err("keys cannot be empty".to_string());
    }
    let mut sequence = Vec::new();
    let mut shift_held = false;
    for name in names {
        for stroke in strokes_for_name(name)? {
            if stroke.vk == VK_SHIFT || stroke.vk == VK_RSHIFT {
                if shift_held {
                    continue;
                }
                shift_held = true;
            }
            sequence.push(stroke);
        }
    }
    let mut input = Vec::with_capacity(sequence.len() * 2);
    for stroke in &sequence {
        input.push(key_event(*stroke, false));
    }
    for stroke in sequence.iter().rev() {
        input.push(key_event(*stroke, true));
    }
    send_input(&input)
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

struct ClipboardLock;

impl ClipboardLock {
    fn open() -> Result<Self, String> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Self);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err("Clipboard is busy".to_string())
    }
}

impl Drop for ClipboardLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn with_clipboard_text(
    text: &str,
    body: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let previous = {
        let _lock = ClipboardLock::open()?;
        read_unicode_clipboard()?
    };
    if let Err(error) = (|| {
        let _lock = ClipboardLock::open()?;
        set_unicode_clipboard(text)
    })() {
        restore_clipboard(previous.as_deref())?;
        return Err(error);
    }
    let outcome = body();
    let restored = restore_clipboard(previous.as_deref());
    outcome.and(restored)
}

fn restore_clipboard(previous: Option<&str>) -> Result<(), String> {
    let _lock = ClipboardLock::open()?;
    match previous {
        Some(previous) => set_unicode_clipboard(previous),
        None => unsafe { EmptyClipboard() }
            .map_err(|error| format!("Unable to restore the clipboard: {error}")),
    }
}

fn read_unicode_clipboard() -> Result<Option<String>, String> {
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_err() {
        return Ok(None);
    }
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) }
        .map_err(|error| format!("Unable to read the clipboard: {error}"))?;
    if handle.is_invalid() {
        return Ok(None);
    }
    let memory = windows::Win32::Foundation::HGLOBAL(handle.0);
    let pointer = unsafe { GlobalLock(memory) };
    if pointer.is_null() {
        return Err("Unable to read the clipboard".to_string());
    }
    let units = (unsafe { GlobalSize(memory) } / 2).saturating_sub(1);
    let text = unsafe {
        let data = std::slice::from_raw_parts(pointer as *const u16, units);
        let end = data
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(data.len());
        String::from_utf16_lossy(&data[..end])
    };
    unsafe {
        let _ = GlobalUnlock(memory);
    }
    Ok(Some(text))
}

fn set_unicode_clipboard(text: &str) -> Result<(), String> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    let bytes = units.len() * 2;
    if bytes > MAX_PASTE_BYTES {
        return Err("Text is too large to paste".to_string());
    }
    unsafe { EmptyClipboard() }
        .map_err(|error| format!("Unable to prepare the clipboard: {error}"))?;
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
        .map_err(|error| format!("Unable to prepare the clipboard: {error}"))?;
    let pointer = unsafe { GlobalLock(memory) };
    if pointer.is_null() {
        return Err("Unable to prepare the clipboard".to_string());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer as *mut u16, units.len());
        let _ = GlobalUnlock(memory);
        SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0)))
            .map_err(|error| format!("Unable to set the clipboard: {error}"))?;
    }
    Ok(())
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
    fn clipboard_text_is_restored_after_use() {
        let previous = {
            let _lock = ClipboardLock::open().unwrap();
            read_unicode_clipboard().unwrap()
        };
        let outcome = with_clipboard_text("crabcode-clipboard-probe", || Ok(()));
        let restored = {
            let _lock = ClipboardLock::open().unwrap();
            read_unicode_clipboard().unwrap()
        };
        assert!(outcome.is_ok());
        assert_eq!(restored, previous);
    }
}
