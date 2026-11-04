//! The Windows side of the catalog: windows, thumbnails, icons, apps and window commands.

use crate::protocol::{AppInfo, WindowInfo};
use windows::core::{w, PCWSTR, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Monitors other than the main one are listed like windows, from this ID up.
const DISPLAY_BASE: u32 = 0xFFFF_0000;

unsafe extern "system" fn add_monitor(_: HMONITOR, _: HDC, r: *mut RECT, list: LPARAM) -> BOOL {
    (*(list.0 as *mut Vec<RECT>)).push(*r);
    TRUE
}

/// The extra monitors' rectangles, in Windows' order, the main one left out.
fn displays() -> Vec<RECT> {
    let mut all: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(add_monitor), LPARAM(&mut all as *mut _ as isize));
    }
    all.retain(|r| r.left != 0 || r.top != 0);
    all
}

fn display(id: u32) -> Option<RECT> {
    id.checked_sub(DISPLAY_BASE).and_then(|i| displays().get(i as usize).copied())
}

fn hwnd(id: u32) -> HWND {
    HWND(id as usize as *mut _)
}

unsafe fn text(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = GetWindowTextW(h, &mut b).max(0) as usize;
    String::from_utf16_lossy(&b[..n])
}

unsafe fn class(h: HWND) -> String {
    let mut b = [0u16; 128];
    let n = GetClassNameW(h, &mut b).max(0) as usize;
    String::from_utf16_lossy(&b[..n])
}

unsafe fn program(h: HWND) -> Option<String> {
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, Some(&mut pid));
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut b = [0u16; 1024];
    let mut n = b.len() as u32;
    let r = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(b.as_mut_ptr()), &mut n);
    let _ = CloseHandle(process);
    r.ok()?;
    Some(String::from_utf16_lossy(&b[..n as usize]))
}

/// The name a program gives itself ("Google Chrome"), else its file's name, capitalized.
fn app_name(path: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::*;
    if path.is_empty() {
        return None;
    }
    let description = unsafe {
        let wide = HSTRING::from(path);
        let size = GetFileVersionInfoSizeW(&wide, None);
        let mut data = vec![0u8; size as usize];
        let mut found = None;
        if size > 0 && GetFileVersionInfoW(&wide, 0, size, data.as_mut_ptr() as *mut _).is_ok() {
            let (mut p, mut n) = (std::ptr::null_mut(), 0u32);
            // The first language the file lists, as (language, code page) pairs.
            if VerQueryValueW(data.as_ptr() as *const _, w!("\\VarFileInfo\\Translation"), &mut p, &mut n).as_bool() && n >= 4 {
                let t = std::slice::from_raw_parts(p as *const u16, 2);
                let key = HSTRING::from(format!("\\StringFileInfo\\{:04x}{:04x}\\FileDescription", t[0], t[1]));
                if VerQueryValueW(data.as_ptr() as *const _, &key, &mut p, &mut n).as_bool() && n > 1 {
                    let units = std::slice::from_raw_parts(p as *const u16, n as usize - 1);
                    found = Some(String::from_utf16_lossy(units).trim_end_matches('\0').trim().to_string());
                }
            }
        }
        found
    };
    description.filter(|d| !d.is_empty()).or_else(|| {
        let stem = std::path::Path::new(path).file_stem()?.to_string_lossy().to_string();
        let mut c = stem.chars();
        Some(c.next()?.to_uppercase().collect::<String>() + c.as_str())
    })
}

/// The visible part of a window on the desktop, without the invisible resize border.
pub fn bounds(id: u32) -> Option<RECT> {
    if id >= DISPLAY_BASE {
        return display(id);
    }
    unsafe {
        let h = hwnd(id);
        if !IsWindow(h).as_bool() || IsIconic(h).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32).ok()?;
        Some(r)
    }
}

unsafe extern "system" fn add(h: HWND, list: LPARAM) -> BOOL {
    let list = &mut *(list.0 as *mut Vec<HWND>);
    list.push(h);
    TRUE
}

pub fn windows() -> Vec<WindowInfo> {
    let mut all: Vec<HWND> = Vec::new();
    let mut out = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(add), LPARAM(&mut all as *mut _ as isize));
        for h in all {
            if !IsWindowVisible(h).as_bool() || GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
                continue;
            }
            if GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
                continue;
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
            if cloaked != 0 {
                continue;
            }
            let (title, c) = (text(h), class(h));
            if title.is_empty() || c == "Progman" || c == "WorkerW" || c == "SillPairWindow" {
                continue;
            }
            let id = h.0 as usize as u32;
            // Its normal size, so a minimized window stays listed with its real one.
            let mut place = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            if GetWindowPlacement(h, &mut place).is_err() {
                continue;
            }
            let r = place.rcNormalPosition;
            let (width, height) = ((r.right - r.left) as i64, (r.bottom - r.top) as i64);
            if width < 100 || height < 50 {
                continue;
            }
            let path = program(h).unwrap_or_default();
            let app_name = app_name(&path).unwrap_or_else(|| title.clone());
            out.push(WindowInfo { id, title, app_name, bundle_id: path, width, height });
        }
    }
    for (i, r) in displays().iter().enumerate() {
        out.push(WindowInfo {
            id: DISPLAY_BASE + i as u32,
            title: format!("Display {}", i + 2),
            app_name: "Displays".into(),
            bundle_id: "sill:display".into(),
            width: (r.right - r.left) as i64,
            height: (r.bottom - r.top) as i64,
        });
    }
    out
}

/// Bottom-up off: a 32-bit top-down bitmap header.
fn header(w: i32, h: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn thumbnail(id: u32) -> Option<Vec<u8>> {
    unsafe {
        let h = hwnd(id);
        let shown = display(id);
        if shown.is_none() && IsIconic(h).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        match shown {
            Some(d) => r = d,
            None => GetWindowRect(h, &mut r).ok()?,
        }
        let (w, ht) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || ht <= 0 {
            return None;
        }
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let info = header(w, ht);
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0);
        let mut out = None;
        if let Ok(bmp) = bmp {
            let old = SelectObject(dc, bmp);
            // 2: PW_RENDERFULLCONTENT, which also catches GPU-drawn windows such as browsers.
            let drawn = if shown.is_some() {
                BitBlt(dc, 0, 0, w, ht, screen, r.left, r.top, SRCCOPY).is_ok()
            } else {
                PrintWindow(h, dc, PRINT_WINDOW_FLAGS(2)).as_bool()
            };
            if drawn && !bits.is_null() {
                let px = std::slice::from_raw_parts(bits as *const u8, w as usize * ht as usize * 4);
                out = super::thumbnail_jpeg(px, w as usize, ht as usize, 480);
            }
            SelectObject(dc, old);
            let _ = DeleteObject(bmp);
        }
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        out
    }
}

pub fn icon(path: &str) -> Option<Vec<u8>> {
    if path == "sill:display" {
        return None;
    }
    unsafe {
        let mut info = SHFILEINFOW::default();
        let wide = HSTRING::from(path);
        let got = SHGetFileInfoW(&wide, FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON);
        if got == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let png = icon_png(info.hIcon);
        let _ = DestroyIcon(info.hIcon);
        png
    }
}

unsafe fn icon_png(icon: HICON) -> Option<Vec<u8>> {
    let mut ii = ICONINFO::default();
    GetIconInfo(icon, &mut ii).ok()?;
    let color = ii.hbmColor;
    let mut out = None;
    if !color.is_invalid() {
        let mut bm = BITMAP::default();
        GetObjectW(color, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let mut px = vec![0u8; (w * h * 4).max(0) as usize];
        let mut info = header(w, h);
        let dc = GetDC(None);
        let lines = GetDIBits(dc, color, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if lines == h {
            // Old icons have no alpha at all: draw those solid rather than invisible.
            let any_alpha = px.chunks(4).any(|p| p[3] != 0);
            for p in px.chunks_mut(4) {
                p.swap(0, 2);
                if !any_alpha {
                    p[3] = 255;
                }
            }
            out = crate::cursor::png(w as u32, h as u32, &px);
        }
        let _ = DeleteObject(color);
    }
    if !ii.hbmMask.is_invalid() {
        let _ = DeleteObject(ii.hbmMask);
    }
    out
}

/// The Start menu's shortcuts, for "All apps". Store apps aren't shortcuts, so they aren't here.
pub fn apps() -> Vec<AppInfo> {
    fn walk(dir: &std::path::Path, depth: u32, out: &mut Vec<AppInfo>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 3 {
                walk(&p, depth + 1, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
                let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let lower = name.to_lowercase();
                if !name.is_empty() && !lower.contains("uninstall") && !lower.contains("readme") {
                    out.push(AppInfo { name, bundle_id: p.to_string_lossy().to_string() });
                }
            }
        }
    }
    let mut out = Vec::new();
    for root in ["ProgramData", "APPDATA"] {
        if let Some(r) = std::env::var_os(root) {
            walk(&std::path::Path::new(&r).join("Microsoft\\Windows\\Start Menu\\Programs"), 0, &mut out);
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    out.truncate(200);
    out
}

pub fn launch(path: &str) -> bool {
    unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL).0 as isize > 32 }
}

/// Brings a window forward, since a window streams by showing it on the screen.
pub fn focus(id: u32) {
    if id >= DISPLAY_BASE {
        return;
    }
    unsafe {
        let h = hwnd(id);
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        // Windows refuses foreground changes from background programs; the second call is the
        // switch Alt+Tab itself uses.
        if !SetForegroundWindow(h).as_bool() {
            SwitchToThisWindow(h, true);
        }
    }
}

pub fn command(id: u32, action: &str) {
    if id >= DISPLAY_BASE {
        return;
    }
    unsafe {
        let h = hwnd(id);
        match action {
            "close" => {
                let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            "minimize" => {
                let _ = ShowWindow(h, SW_MINIMIZE);
            }
            "fullScreen" => {
                let _ = ShowWindow(h, if IsZoomed(h).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
            }
            _ => {}
        }
    }
}

/// Sizes a window so its content suits the device's panel (the Aa control), in Windows' logical
/// pixels, kept on the main monitor.
pub fn fit(id: u32, width: f64, height: f64) {
    if id >= DISPLAY_BASE {
        return;
    }
    unsafe {
        let h = hwnd(id);
        let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(h).max(96) as f64;
        let (sw, sh) = (GetSystemMetrics(SM_CXMAXIMIZED) as f64, GetSystemMetrics(SM_CYMAXIMIZED) as f64);
        let (w, ht) = ((width * dpi / 96.0).clamp(320.0, sw), (height * dpi / 96.0).clamp(240.0, sh));
        let mut r = RECT::default();
        if GetWindowRect(h, &mut r).is_ok() {
            let (x, y) = (r.left.clamp(0, (sw - w) as i32), r.top.clamp(0, (sh - ht) as i32));
            let _ = SetWindowPos(h, None, x, y, w as i32, ht as i32, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

pub fn pointer() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x, p.y))
}
