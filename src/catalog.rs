//! What a device's Apps drawer shows: the PC's windows with thumbnails, app icons, and the
//! installed apps. Kept fresh by one thread while a device is connected. Window IDs are the windows'
//! handles; an app's "bundle ID" is the path of its program or Start menu shortcut, which is
//! only ever launched if it came from this PC's own list.

use crate::protocol::{self as p, kind, AppInfo, WindowInfo};
use crate::server::Host;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

#[cfg(windows)]
#[path = "catalog_windows.rs"]
pub mod platform;

/// A JPEG of a window (BGRA, top row first), scaled to at most `limit` wide.
pub fn thumbnail_jpeg(bgra: &[u8], width: usize, height: usize, limit: usize) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || bgra.len() < width * height * 4 {
        return None;
    }
    let step = width.div_ceil(limit).max(1);
    let (w, h) = (width / step, height / step);
    if w == 0 || h == 0 {
        return None;
    }
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let i = (y * step * width + x * step) * 4;
            rgb.extend_from_slice(&[bgra[i + 2], bgra[i + 1], bgra[i]]);
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 70).encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb).ok()?;
    Some(out)
}

/// A list in Sill's order: first seen first. Windows that went are dropped, new ones go at the end.
pub fn stable_order(old: &[WindowInfo], mut new: Vec<WindowInfo>) -> Vec<WindowInfo> {
    let mut out = Vec::with_capacity(new.len());
    for o in old {
        if let Some(i) = new.iter().position(|w| w.id == o.id) {
            out.push(new.remove(i));
        }
    }
    out.extend(new);
    out
}

/// Outside Windows (tests): with `SILL_TEST_CATALOG`, two made-up windows and apps, so the real
/// app's drawer runs against the real messages.
#[cfg(not(windows))]
pub mod platform {
    use crate::protocol::{AppInfo, WindowInfo};

    fn on() -> bool {
        std::env::var_os("SILL_TEST_CATALOG").is_some()
    }
    pub fn windows() -> Vec<WindowInfo> {
        if !on() {
            return vec![];
        }
        [(101, "Report.docx - Word", "Word", "C:\\apps\\word.exe"), (102, "Inbox - Mail", "Mail", "C:\\apps\\mail.exe")]
            .map(|(id, t, a, b)| WindowInfo { id, title: t.into(), app_name: a.into(), bundle_id: b.into(), width: 1280, height: 720 })
            .to_vec()
    }
    pub fn thumbnail(id: u32) -> Option<Vec<u8>> {
        let c = if id == 101 { [200u8, 80, 40, 255] } else { [40, 160, 80, 255] };
        super::thumbnail_jpeg(&c.repeat(320 * 180), 320, 180, 320)
    }
    pub fn icon(_: &str) -> Option<Vec<u8>> {
        crate::cursor::png(32, 32, &[255u8, 120, 0, 255].repeat(32 * 32))
    }
    pub fn apps() -> Vec<AppInfo> {
        if on() { vec![AppInfo { name: "Calculator".into(), bundle_id: "C:\\apps\\calc.lnk".into() }] } else { vec![] }
    }
    pub fn launch(path: &str) -> bool {
        crate::say!("(test) launch {path}");
        true
    }
    pub fn focus(id: u32) {
        crate::say!("(test) focus window {id}");
    }
    pub fn command(id: u32, action: &str) {
        crate::say!("(test) window {id}: {action}");
    }
    pub fn fit(id: u32, width: f64, height: f64) {
        crate::say!("(test) fit window {id} to {width}×{height} points");
    }
    pub fn pointer() -> Option<(i32, i32)> {
        None
    }
    pub fn exists(_: u32) -> bool {
        false
    }
    pub fn locked() -> bool {
        false
    }
}

/// Keeps the window list and icons fresh while anyone is connected.
pub fn start(host: Arc<Host>) {
    start_thumbnails(host.clone());
    std::thread::Builder::new()
        .name("catalog".into())
        .spawn(move || {
            #[cfg(windows)]
            unsafe {
                // The shell's icon calls need COM on this thread.
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            let hub = host.hub.clone();
            let apps = platform::apps();
            *hub.app_paths.lock().unwrap() = apps.iter().map(|a: &AppInfo| a.bundle_id.clone()).collect();
            let mut sent_icons: HashMap<String, bool> = HashMap::new();
            for a in &apps {
                remember_icon(&host, &a.bundle_id, &mut sent_icons);
            }
            let list = Arc::new(p::json_message(kind::APP_LIST, &apps));
            *hub.app_list.lock().unwrap() = Some(list.clone());
            hub.send_all((*list).clone()); // a device that connected during the scan
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let locked = platform::locked();
                let was = hub.locked.swap(locked, Ordering::SeqCst);
                if locked != was {
                    crate::say!("This PC {}.", if locked { "is locked: devices see nothing of it until it's unlocked" } else { "is unlocked" });
                    hub.send_all(host.window_list());
                }
                if hub.has_menu_subscribers() {
                    hub.refresh_menus();
                }
                if hub.client_count() == 0 || locked {
                    continue;
                }
                let old = hub.windows.lock().unwrap().clone();
                let source = hub.source.lock().unwrap().clone();
                let mut list = stable_order(&old, platform::windows());
                // One look can miss a window (a title changing, say): the one streaming stays
                // listed for as long as it really exists.
                if let p::StreamSource::Window { id } = source {
                    if !list.iter().any(|w| w.id == id) && platform::exists(id) {
                        list.extend(old.iter().filter(|w| w.id == id).cloned());
                    }
                }
                for w in &list {
                    remember_icon(&host, &w.bundle_id, &mut sent_icons);
                }
                if serde_json::to_vec(&list).ok() != serde_json::to_vec(&old).ok() {
                    *hub.windows.lock().unwrap() = list.clone();
                    // A streamed window that closed: back to the whole screen.
                    if let p::StreamSource::Window { id } = source {
                        if !list.iter().any(|w| w.id == id) {
                            hub.set_source(p::StreamSource::Desktop {});
                        }
                    }
                    // An app a device opened: its first new window streams.
                    let launching = hub.launching.lock().unwrap().clone();
                    if let Some((at, known)) = launching {
                        if p::now() - at > 15.0 {
                            *hub.launching.lock().unwrap() = None;
                        } else if let Some(new) = list.iter().find(|w| !known.contains(&w.id)) {
                            *hub.launching.lock().unwrap() = None;
                            platform::focus(new.id);
                            hub.set_source(p::StreamSource::Window { id: new.id });
                        }
                    }
                    hub.send_all(host.window_list());
                }
            }
        })
        .expect("catalog thread");
}

/// Thumbnails, on a thread of their own: drawing one waits on its program, and a program that
/// has just frozen mustn't stop the window list.
fn start_thumbnails(host: Arc<Host>) {
    std::thread::Builder::new()
        .name("thumbnails".into())
        .spawn(move || {
            let hub = host.hub.clone();
            let mut sums: HashMap<u32, u64> = HashMap::new();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if hub.client_count() == 0 || hub.locked.load(Ordering::SeqCst) {
                    continue;
                }
                let list = hub.windows.lock().unwrap().clone();
                sums.retain(|id, _| list.iter().any(|w| w.id == *id));
                hub.thumbnails.lock().unwrap().retain(|id, _| list.iter().any(|w| w.id == *id));
                for w in list.iter().take(16) {
                    let Some(jpeg) = platform::thumbnail(w.id) else { continue };
                    let sum = ring::digest::digest(&ring::digest::SHA256, &jpeg);
                    let sum = u64::from_be_bytes(sum.as_ref()[..8].try_into().unwrap());
                    if sums.insert(w.id, sum) != Some(sum) {
                        let m = p::message(kind::THUMBNAIL, p::now(), false, &p::encode_thumbnail(w.id, &jpeg));
                        hub.thumbnails.lock().unwrap().insert(w.id, Arc::new(m.clone()));
                        hub.send_all(m);
                    }
                }
            }
        })
        .expect("thumbnail thread");
}

fn remember_icon(host: &Host, path: &str, seen: &mut HashMap<String, bool>) {
    if seen.contains_key(path) {
        return;
    }
    let icon = platform::icon(path);
    seen.insert(path.to_string(), icon.is_some());
    if let Some(png) = icon {
        let m = Arc::new(p::message(kind::APP_ICON, p::now(), false, &p::encode_icon(path, &png)));
        host.hub.icons.lock().unwrap().push(m.clone());
        host.hub.send_all((*m).clone());
    }
}

/// Where the PC's own pointer is, for devices that aren't the ones moving it, sampled 30 times a second.
pub fn start_pointer(hub: Arc<crate::server::Hub>, rect: Arc<std::sync::Mutex<crate::input::Rect>>) {
    std::thread::Builder::new()
        .name("pointer".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_millis(33));
            if hub.client_count() == 0 {
                continue;
            }
            let Some((x, y)) = platform::pointer() else { continue };
            let r = *rect.lock().unwrap();
            let inside = x >= r.left && y >= r.top && x < r.left + r.width && y < r.top + r.height;
            let round = |v: f64| (v * 10_000.0).round() / 10_000.0;
            let place = inside.then(|| (round((x - r.left) as f64 / r.width.max(1) as f64), round((y - r.top) as f64 / r.height.max(1) as f64)));
            hub.pointer_sample(place);
        })
        .expect("pointer thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u32) -> WindowInfo {
        WindowInfo { id, title: String::new(), app_name: String::new(), bundle_id: String::new(), width: 1, height: 1 }
    }

    #[test]
    fn first_seen_first() {
        let order = stable_order(&[w(3), w(1), w(2)], vec![w(1), w(4), w(3)]);
        assert_eq!(order.iter().map(|w| w.id).collect::<Vec<_>>(), vec![3, 1, 4]);
    }

    #[test]
    fn thumbnails_are_jpegs() {
        let j = thumbnail_jpeg(&[10, 20, 30, 255].repeat(1000 * 500), 1000, 500, 320).unwrap();
        assert_eq!(&j[..2], &[0xFF, 0xD8]);
        assert!(thumbnail_jpeg(&[0; 4], 10, 10, 320).is_none(), "short data is refused");
    }
}
