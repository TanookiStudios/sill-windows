//! The clipboard (kind 32, text): what a device copies lands on Windows' clipboard, and text copied
//! on Windows goes to every device that subscribed. The rules are ClipboardPolicy.swift's: at most
//! 256 KB of UTF-8, a device's own text never echoed back to it, at most 4 subscriptions or texts a
//! second per connection, a stop always taken.

use std::sync::atomic::{AtomicU32, Ordering};

pub const MAX_TEXT_BYTES: usize = 256 * 1024;
const RATE: usize = 4;
/// The clipboard's sequence number after this host's own write, so it isn't sent back out.
static OWN: AtomicU32 = AtomicU32::new(0);

/// What a device's kind 32 asks (ClipboardPolicy.fromDevice).
#[derive(Debug, PartialEq)]
pub enum Ask {
    Subscribe,
    Set(String),
    Stop,
    Refuse,
}

pub fn from_device(payload: &[u8]) -> Ask {
    let Ok(clip) = serde_json::from_slice::<crate::protocol::Clipboard>(payload) else { return Ask::Refuse };
    if clip.stop == Some(true) {
        return Ask::Stop;
    }
    match clip.text {
        Some(t) if !t.is_empty() => {
            if t.len() <= MAX_TEXT_BYTES { Ask::Set(t) } else { Ask::Refuse }
        }
        _ => Ask::Subscribe,
    }
}

/// The rate of a connection's subscriptions and texts: whether one at `now` is taken.
pub fn admits(arrivals: &mut Vec<f64>, now: f64) -> bool {
    arrivals.retain(|t| now - *t < 1.0);
    if arrivals.len() >= RATE {
        return false;
    }
    arrivals.push(now);
    true
}

/// Text to put on this PC's clipboard.
pub fn write(text: &str) -> bool {
    let done = platform::write(text);
    if done {
        OWN.store(platform::sequence(), Ordering::SeqCst);
    }
    done
}

/// Sends text copied on this PC to the subscribed devices, looking four times a second while any
/// device subscribes (Windows bumps a counter at every copy, so a look costs nothing).
pub fn start(hub: std::sync::Arc<crate::server::Hub>) {
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || {
            let mut seen = platform::sequence();
            let mut watching = false;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if !hub.has_clipboard_subscribers() {
                    watching = false;
                    continue;
                }
                let now = platform::sequence();
                if !watching {
                    watching = true; // what was copied before anyone subscribed stays private
                    seen = now;
                    continue;
                }
                if now == seen {
                    continue;
                }
                seen = now;
                if now == OWN.load(Ordering::SeqCst) {
                    continue;
                }
                if let Some(text) = platform::read() {
                    if !text.is_empty() && text.len() <= MAX_TEXT_BYTES {
                        hub.send_clipboard(&text, None);
                    }
                }
            }
        })
        .expect("clipboard thread");
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::*;
    const CF_UNICODETEXT: u32 = 13;

    pub fn sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    /// Another program can hold the clipboard for a moment.
    unsafe fn open() -> bool {
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    pub fn read() -> Option<String> {
        unsafe {
            if !open() {
                return None;
            }
            let mut text = None;
            if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
                let g = HGLOBAL(h.0);
                let p = GlobalLock(g) as *const u16;
                if !p.is_null() {
                    let limit = GlobalSize(g) / 2;
                    let units = std::slice::from_raw_parts(p, limit);
                    let end = units.iter().position(|&u| u == 0).unwrap_or(limit);
                    text = String::from_utf16(&units[..end]).ok();
                    let _ = GlobalUnlock(g);
                }
            }
            let _ = CloseClipboard();
            text
        }
    }

    pub fn write(text: &str) -> bool {
        let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
        unsafe {
            if !open() {
                return false;
            }
            let mut done = false;
            if EmptyClipboard().is_ok() {
                if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) {
                    let p = GlobalLock(g) as *mut u16;
                    if !p.is_null() {
                        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
                        let _ = GlobalUnlock(g);
                        // The clipboard owns the memory from here on.
                        done = SetClipboardData(CF_UNICODETEXT, HANDLE(g.0)).is_ok();
                    }
                }
            }
            let _ = CloseClipboard();
            done
        }
    }
}

// Outside Windows (tests): a file stands in for the clipboard, its modification counted as a copy.
#[cfg(not(windows))]
mod platform {
    use std::sync::atomic::{AtomicU32, Ordering};
    static BUMP: AtomicU32 = AtomicU32::new(1);

    fn file() -> Option<std::path::PathBuf> {
        std::env::var_os("SILL_TEST_CLIPBOARD").map(Into::into)
    }

    pub fn sequence() -> u32 {
        let stamp = file().and_then(|f| std::fs::metadata(f).ok()).and_then(|m| m.modified().ok());
        let ms = stamp.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u32).unwrap_or(0);
        ms.wrapping_add(BUMP.load(Ordering::SeqCst))
    }

    pub fn read() -> Option<String> {
        file().and_then(|f| std::fs::read_to_string(f).ok())
    }

    pub fn write(text: &str) -> bool {
        BUMP.fetch_add(1, Ordering::SeqCst);
        file().map(|f| std::fs::write(f, text).is_ok()).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_device_asks() {
        assert_eq!(from_device(b"{}"), Ask::Subscribe);
        assert_eq!(from_device(br#"{"text":""}"#), Ask::Subscribe);
        assert_eq!(from_device(br#"{"text":"hi"}"#), Ask::Set("hi".into()));
        assert_eq!(from_device(br#"{"text":"hi","stop":true}"#), Ask::Stop, "stop wins");
        assert_eq!(from_device(b"not json"), Ask::Refuse);
        let long = format!(r#"{{"text":"{}"}}"#, "x".repeat(MAX_TEXT_BYTES + 1));
        assert_eq!(from_device(long.as_bytes()), Ask::Refuse);
    }

    #[test]
    fn four_a_second() {
        let mut a = Vec::new();
        assert!((0..4).all(|i| admits(&mut a, 10.0 + i as f64 / 10.0)));
        assert!(!admits(&mut a, 10.5));
        assert!(admits(&mut a, 11.1));
    }
}
