use std::ffi::c_void;
use std::thread;
use std::time::{Duration, Instant};

type Hwnd = *mut c_void;

#[link(name = "user32")]
extern "system" {
    fn IsWindow(window: Hwnd) -> i32;
    fn IsIconic(window: Hwnd) -> i32;
    fn ShowWindowAsync(window: Hwnd, command: i32) -> i32;
    fn SetForegroundWindow(window: Hwnd) -> i32;
    fn GetForegroundWindow() -> Hwnd;
}

pub(super) fn focus_window(id: u32) -> Result<(), String> {
    // xcap exposes the Windows HWND as a u32 window ID.
    let window = id as usize as Hwnd;
    // SAFETY: User32 validates opaque window handles; no pointer is dereferenced here.
    unsafe {
        if IsWindow(window) == 0 {
            return Err(format!("Window not found: {id}"));
        }
        if IsIconic(window) != 0 && ShowWindowAsync(window, 9 /* SW_RESTORE */) == 0 {
            return Err(format!("Unable to restore window {id}"));
        }
        // Foreground activation and restoration can complete asynchronously.
        SetForegroundWindow(window);
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if IsWindow(window) == 0 {
                return Err(format!("Window closed while focusing: {id}"));
            }
            if GetForegroundWindow() == window && IsIconic(window) == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "Unable to focus window {id}: target is not foreground or is still minimized; Windows may have denied foreground activation"
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_window_is_not_reported_as_focused() {
        assert_eq!(focus_window(0), Err("Window not found: 0".to_string()));
    }
}
