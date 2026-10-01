use std::io;
use std::sync::{Mutex, OnceLock};

fn error_queue() -> &'static Mutex<Vec<String>> {
    static QUEUE: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Queues a clipboard write on a background thread so a large yank never
/// blocks the editing loop. Failures are collected for [`take_errors`].
pub fn queue_contents(text: &str) -> io::Result<()> {
    let owned = text.to_string();
    std::thread::Builder::new()
        .name("wed-clipboard".to_string())
        .spawn(move || {
            if let Err(error) = set_contents(&owned) {
                if let Ok(mut queue) = error_queue().lock() {
                    queue.push(error.to_string());
                }
            }
        })
        .map(|_| ())
        .map_err(|error| io::Error::other(format!("clipboard thread failed: {error}")))
}

/// Drains clipboard failures reported by background writes.
pub fn take_errors() -> Vec<String> {
    error_queue()
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

pub fn set_contents(text: &str) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        win32::set_text(text)
    }

    #[cfg(target_os = "macos")]
    {
        set_with_command(text, "pbcopy", &[])
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match set_with_command(text, "wl-copy", &[]) {
            Ok(()) => Ok(()),
            Err(wayland_error) => set_with_command(text, "xclip", &["-selection", "clipboard"])
                .map_err(|xclip_error| {
                    io::Error::other(format!(
                        "wl-copy failed: {wayland_error}; xclip failed: {xclip_error}"
                    ))
                }),
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::other("system clipboard is not supported"))
    }
}

pub fn get_contents() -> String {
    #[cfg(target_os = "windows")]
    {
        win32::get_text()
    }

    #[cfg(target_os = "macos")]
    {
        get_with_command("pbpaste", &[]).unwrap_or_default()
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        get_with_command("wl-paste", &["--no-newline"])
            .or_else(|_| get_with_command("xclip", &["-selection", "clipboard", "-o"]))
            .unwrap_or_default()
    }

    #[cfg(not(any(unix, windows)))]
    {
        String::new()
    }
}

#[cfg(unix)]
fn set_with_command(text: &str, program: &str, args: &[&str]) -> io::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()?;
    let write_result = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(text.as_bytes()),
        None => Err(io::Error::other("clipboard command stdin was not piped")),
    };
    let status_result = child.wait();
    write_result?;
    let status = status_result?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{program} exited with {status}")))
    }
}

#[cfg(unix)]
fn get_with_command(program: &str, args: &[&str]) -> io::Result<String> {
    use std::process::Command;

    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{program} exited with {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;
    use std::io;
    use std::ptr::null_mut;

    const GMEM_MOVEABLE: u32 = 0x0002;
    const CF_UNICODETEXT: u32 = 13;

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(uFlags: u32, dwBytes: usize) -> *mut c_void;
        fn GlobalFree(hMem: *mut c_void) -> *mut c_void;
        fn GlobalLock(hMem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(hMem: *mut c_void) -> i32;
        fn RtlCopyMemory(Destination: *mut c_void, Source: *const c_void, Length: usize);
    }

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hWndNewOwner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(uFormat: u32, hMem: *mut c_void) -> *mut c_void;
        fn GetClipboardData(uFormat: u32) -> *mut c_void;
    }

    pub fn set_text(text: &str) -> io::Result<()> {
        let utf16: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes_len = utf16.len() * 2;
        unsafe {
            if OpenClipboard(null_mut()) == 0 {
                return Err(io::Error::last_os_error());
            }
            let result = (|| -> io::Result<()> {
                if EmptyClipboard() == 0 {
                    return Err(io::Error::last_os_error());
                }
                let h_mem = GlobalAlloc(GMEM_MOVEABLE, bytes_len);
                if h_mem.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let ptr = GlobalLock(h_mem);
                if ptr.is_null() {
                    let _ = GlobalFree(h_mem);
                    return Err(io::Error::last_os_error());
                }
                RtlCopyMemory(ptr, utf16.as_ptr() as *const c_void, bytes_len);
                let _ = GlobalUnlock(h_mem);
                if SetClipboardData(CF_UNICODETEXT, h_mem).is_null() {
                    let _ = GlobalFree(h_mem);
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })();
            let _ = CloseClipboard();
            result
        }
    }

    pub fn get_text() -> String {
        let mut result = String::new();
        unsafe {
            if OpenClipboard(null_mut()) != 0 {
                let h_mem = GetClipboardData(CF_UNICODETEXT);
                if !h_mem.is_null() {
                    let ptr = GlobalLock(h_mem) as *const u16;
                    if !ptr.is_null() {
                        let mut len = 0;
                        while *ptr.add(len) != 0 {
                            len += 1;
                        }
                        let slice = std::slice::from_raw_parts(ptr, len);
                        result = String::from_utf16_lossy(slice);
                        let _ = GlobalUnlock(h_mem);
                    }
                }
                let _ = CloseClipboard();
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::take_errors;

    #[test]
    fn error_queue_starts_empty_and_drains() {
        assert!(take_errors().is_empty());
    }
}
