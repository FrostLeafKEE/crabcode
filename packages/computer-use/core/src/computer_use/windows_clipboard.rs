//! Preserve every clipboard format while briefly staging a text paste.
use std::{thread, time::Duration};
use windows::core::w;
use windows::Win32::Foundation::{
    GetLastError, GlobalFree, SetLastError, ERROR_SUCCESS, HANDLE, HGLOBAL, HWND,
};
use windows::Win32::Graphics::Gdi::{
    CopyEnhMetaFileW, DeleteEnhMetaFile, DeleteMetaFile, DeleteObject, HENHMETAFILE, HGDIOBJ,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardOwner,
    GetClipboardSequenceNumber, OpenClipboard, SetClipboardData, METAFILEPICT,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{OleDuplicateData, CLIPBOARD_FORMAT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};

const CF_UNICODETEXT: u32 = 13;
const MAX_PASTE_BYTES: usize = 1024 * 1024;

struct ClipboardWindow(HWND);

impl ClipboardWindow {
    fn new() -> Result<Self, String> {
        // EmptyClipboard needs a real owner for SetClipboardData to succeed.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .map(Self)
        .map_err(|e| format!("Unable to create clipboard owner: {e}"))
    }

    fn lock(&self) -> Result<ClipboardLock, String> {
        for _ in 0..20 {
            if unsafe { OpenClipboard(Some(self.0)) }.is_ok() {
                return Ok(ClipboardLock);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err("Clipboard is busy".into())
    }
}

impl Drop for ClipboardWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

struct ClipboardLock;
impl Drop for ClipboardLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

struct ClipboardData {
    format: u32,
    handle: HANDLE,
}

impl ClipboardData {
    fn duplicate(format: u32, source: HANDLE) -> Result<Self, String> {
        // Owner-drawn and private formats have application-specific lifetimes.
        // Leave the clipboard untouched if we cannot faithfully preserve them.
        if format == 0x80 || (0x200..=0x2ff).contains(&format) || format > u16::MAX as u32 {
            return Err(format!(
                "Clipboard format {format} cannot be preserved; no text was pasted"
            ));
        }
        let handle = if matches!(format, 14 | 0x8e) {
            HANDLE(unsafe { CopyEnhMetaFileW(HENHMETAFILE(source.0), None) }.0)
        } else {
            let kind = match format {
                0x82 => 2,
                0x83 => 3,
                _ => format,
            };
            unsafe { OleDuplicateData(source, CLIPBOARD_FORMAT(kind as u16), GMEM_MOVEABLE) }
        };
        if handle.is_invalid() {
            return Err(format!(
                "Unable to preserve clipboard format {format}; no text was pasted"
            ));
        }
        Ok(Self { format, handle })
    }

    fn text(text: &str) -> Result<Self, String> {
        let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let bytes = units.len() * 2;
        if bytes > MAX_PASTE_BYTES || text.contains('\0') {
            return Err("Text is too large to paste or contains a NUL character".into());
        }
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
            .map_err(|e| format!("Unable to prepare clipboard text: {e}"))?;
        let data = Self {
            format: CF_UNICODETEXT,
            handle: HANDLE(memory.0),
        };
        let pointer = unsafe { GlobalLock(memory) };
        if pointer.is_null() {
            return Err("Unable to prepare clipboard text".into());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(units.as_ptr(), pointer as *mut u16, units.len());
            let _ = GlobalUnlock(memory);
        }
        Ok(data)
    }

    fn publish(&mut self) -> Result<(), String> {
        unsafe { SetClipboardData(self.format, Some(self.handle)) }
            .map_err(|e| format!("Unable to set clipboard format {}: {e}", self.format))?;
        self.handle = HANDLE::default(); // Ownership passed to Windows.
        Ok(())
    }
}

impl Drop for ClipboardData {
    fn drop(&mut self) {
        if self.handle.is_invalid() {
            return;
        }
        unsafe {
            match self.format {
                2 | 9 | 0x82 => {
                    let _ = DeleteObject(HGDIOBJ(self.handle.0));
                }
                14 | 0x8e => {
                    let _ = DeleteEnhMetaFile(Some(HENHMETAFILE(self.handle.0)));
                }
                format => {
                    let memory = HGLOBAL(self.handle.0);
                    if matches!(format, 3 | 0x83) {
                        let pointer = GlobalLock(memory) as *const METAFILEPICT;
                        if !pointer.is_null() {
                            let _ = DeleteMetaFile((*pointer).hMF);
                            let _ = GlobalUnlock(memory);
                        }
                    }
                    let _ = GlobalFree(Some(memory));
                }
            }
        }
    }
}

struct ClipboardSnapshot(Vec<ClipboardData>);

impl ClipboardSnapshot {
    // Caller holds the clipboard open for the entire snapshot or restore.
    fn capture() -> Result<Self, String> {
        let mut items = Vec::new();
        let mut format = 0;
        loop {
            unsafe {
                SetLastError(ERROR_SUCCESS);
            }
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                if unsafe { GetLastError() } != ERROR_SUCCESS {
                    return Err("Unable to enumerate clipboard formats; no text was pasted".into());
                }
                return Ok(Self(items));
            }
            let source = unsafe { GetClipboardData(format) }
                .map_err(|e| format!("Unable to read clipboard format {format}: {e}"))?;
            items.push(ClipboardData::duplicate(format, source)?);
        }
    }

    fn restore(mut self) -> Result<(), String> {
        unsafe { EmptyClipboard() }.map_err(|e| format!("Unable to restore clipboard: {e}"))?;
        let mut error = None;
        for item in &mut self.0 {
            if let Err(e) = item.publish() {
                error.get_or_insert(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
}

pub(super) fn with_clipboard_text(
    text: &str,
    body: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let mut staged = ClipboardData::text(text)?;
    let window = ClipboardWindow::new()?;
    let previous = {
        let _lock = window.lock()?;
        let previous = ClipboardSnapshot::capture()?;
        unsafe { EmptyClipboard() }.map_err(|e| format!("Unable to prepare clipboard: {e}"))?;
        if let Err(error) = staged.publish() {
            previous.restore()?;
            return Err(error);
        }
        previous
    };
    let sequence = unsafe { GetClipboardSequenceNumber() };
    let outcome = body();
    let _lock = window.lock()?;
    // A user/application may copy new data while the paste is being delivered.
    // Never overwrite that newer clipboard with the earlier snapshot.
    if unsafe { GetClipboardOwner() }.ok() == Some(window.0)
        && unsafe { GetClipboardSequenceNumber() } == sequence
    {
        previous.restore()?;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Memory::GlobalSize;

    #[test]
    fn copies_memory_backed_formats_without_sharing_handles() {
        let original = ClipboardData::text("文件 / rich text / image bytes").unwrap();
        for format in [
            CF_UNICODETEXT,
            8,      /* DIB */
            15,     /* file list */
            0xc123, /* registered */
        ] {
            let copy = ClipboardData::duplicate(format, original.handle).unwrap();
            assert_ne!(original.handle, copy.handle);
            unsafe {
                let original_memory = HGLOBAL(original.handle.0);
                let copied_memory = HGLOBAL(copy.handle.0);
                let size = GlobalSize(original_memory);
                assert_eq!(size, GlobalSize(copied_memory));
                let a = GlobalLock(original_memory) as *const u8;
                let b = GlobalLock(copied_memory) as *const u8;
                assert!(!a.is_null() && !b.is_null());
                assert_eq!(
                    std::slice::from_raw_parts(a, size),
                    std::slice::from_raw_parts(b, size)
                );
                let _ = GlobalUnlock(original_memory);
                let _ = GlobalUnlock(copied_memory);
            }
        }
    }

    #[test]
    fn unsupported_formats_and_invalid_text_fail_before_clipboard_changes() {
        let original = ClipboardData::text("original").unwrap();
        assert!(ClipboardData::duplicate(0x80, original.handle).is_err());
        assert!(ClipboardData::duplicate(0x200, original.handle).is_err());
        assert!(ClipboardData::text("embedded\0text").is_err());
        assert!(ClipboardData::text(&"x".repeat(MAX_PASTE_BYTES)).is_err());
    }

    #[test]
    fn clipboard_round_trip_on_private_station() {
        const CHILD: &str = "CRABCODE_PRIVATE_CLIPBOARD_TEST";
        if std::env::var_os(CHILD).is_none() {
            use std::os::windows::process::CommandExt;
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "computer_use::windows_clipboard::tests::clipboard_round_trip_on_private_station", "--nocapture"])
                .env(CHILD, "1").creation_flags(0x08000000).output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        // A child process switches to a fresh window station before any writes.
        // The user's clipboard and the other tests' GUI state remain untouched.
        #[link(name = "user32")]
        extern "system" {
            fn CreateWindowStationW(
                name: *const u16,
                flags: u32,
                access: u32,
                security: *const std::ffi::c_void,
            ) -> *mut std::ffi::c_void;
            fn GetProcessWindowStation() -> *mut std::ffi::c_void;
            fn SetProcessWindowStation(station: *mut std::ffi::c_void) -> i32;
            fn CreateDesktopW(
                name: *const u16,
                device: *const u16,
                mode: *const std::ffi::c_void,
                flags: u32,
                access: u32,
                security: *const std::ffi::c_void,
            ) -> *mut std::ffi::c_void;
            fn SetThreadDesktop(desktop: *mut std::ffi::c_void) -> i32;
        }
        unsafe {
            let original = GetProcessWindowStation();
            let station = CreateWindowStationW(std::ptr::null(), 0, 0x37f, std::ptr::null());
            assert!(!station.is_null(), "{}", std::io::Error::last_os_error());
            assert_ne!(station, original);
            assert_ne!(SetProcessWindowStation(station), 0);
            let desktop = CreateDesktopW(
                w!("ClipboardTest").as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                0x1ff,
                std::ptr::null(),
            );
            assert!(!desktop.is_null(), "{}", std::io::Error::last_os_error());
            assert_ne!(SetThreadDesktop(desktop), 0);
            assert_eq!(GetProcessWindowStation(), station);
        }
        use windows::Win32::Graphics::Gdi::CreateBitmap;
        use windows::Win32::System::DataExchange::{
            IsClipboardFormatAvailable, RegisterClipboardFormatW,
        };
        let owner = ClipboardWindow::new().unwrap();
        let custom = unsafe { RegisterClipboardFormatW(w!("CrabCode test rich text")) };
        assert_ne!(custom, 0);
        {
            let _lock = owner.lock().unwrap();
            unsafe { EmptyClipboard() }.unwrap();
            ClipboardData::text("original text")
                .unwrap()
                .publish()
                .unwrap();
            let mut rich = ClipboardData::text("original rich text").unwrap();
            rich.format = custom;
            rich.publish().unwrap();
            let bitmap = unsafe { CreateBitmap(2, 2, 1, 32, None) };
            assert!(!bitmap.is_invalid());
            ClipboardData {
                format: 2,
                handle: HANDLE(bitmap.0),
            }
            .publish()
            .unwrap();
        }
        fn bytes(format: u32) -> Vec<u8> {
            unsafe {
                let memory = HGLOBAL(GetClipboardData(format).unwrap().0);
                let pointer = GlobalLock(memory) as *const u8;
                assert!(!pointer.is_null());
                let result = std::slice::from_raw_parts(pointer, GlobalSize(memory)).to_vec();
                let _ = GlobalUnlock(memory);
                result
            }
        }
        let (text, rich) = {
            let _lock = owner.lock().unwrap();
            (bytes(CF_UNICODETEXT), bytes(custom))
        };
        for fail in [false, true] {
            let result = with_clipboard_text("temporary paste", || {
                let _lock = owner.lock()?;
                assert_ne!(bytes(CF_UNICODETEXT), text);
                if fail {
                    Err("input failed".into())
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.is_err(), fail);
            let _lock = owner.lock().unwrap();
            assert_eq!(bytes(CF_UNICODETEXT), text);
            assert_eq!(bytes(custom), rich);
            assert!(unsafe { IsClipboardFormatAvailable(2) }.is_ok());
        }
        with_clipboard_text("temporary paste", || {
            let _lock = owner.lock()?;
            unsafe { EmptyClipboard() }.unwrap();
            ClipboardData::text("new user copy").unwrap().publish()
        })
        .unwrap();
        let _lock = owner.lock().unwrap();
        assert_ne!(bytes(CF_UNICODETEXT), text);
        assert!(unsafe { IsClipboardFormatAvailable(custom) }.is_err());
    }
}
