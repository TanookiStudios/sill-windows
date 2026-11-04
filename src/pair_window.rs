//! The pairing window on Windows: the QR code large enough to scan and the 12 digits, in a small
//! window of its own (a console is usually too short for the whole code). It shows when Sill starts
//! with nothing paired and whenever a device asks to pair, and hides once one has paired, so
//! the code isn't on the streamed screen. Closing it only hides it.

pub struct Code {
    pub modules: Vec<bool>,
    pub width: usize,
    pub digits: String,
    pub name: String,
}

#[cfg(windows)]
mod imp {
    use super::Code;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::Mutex;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    use windows::Win32::UI::WindowsAndMessaging::*;

    static CODE: Mutex<Option<Code>> = Mutex::new(None);
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    /// Text height under the code, and the margin around it, at 96 dpi.
    const TEXT: i32 = 150;
    const MARGIN: i32 = 12;
    const QR_SIZE: i32 = 520;
    const SHOW: u32 = WM_APP + 1;
    const HIDE: u32 = WM_APP + 2;

    fn hwnd() -> Option<HWND> {
        match WINDOW.load(Ordering::SeqCst) {
            0 => None,
            h => Some(HWND(h as *mut _)),
        }
    }

    pub fn set(code: Code) {
        *CODE.lock().unwrap() = Some(code);
        if let Some(h) = hwnd() {
            unsafe {
                let _ = InvalidateRect(h, None, true);
            }
        }
    }

    pub fn bring_up() {
        if let Some(h) = hwnd() {
            unsafe {
                let _ = PostMessageW(h, SHOW, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn hide() {
        if let Some(h) = hwnd() {
            unsafe {
                let _ = PostMessageW(h, HIDE, WPARAM(0), LPARAM(0));
            }
        }
    }

    unsafe fn paint(h: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h, &mut ps);
        let guard = CODE.lock().unwrap();
        if let Some(c) = guard.as_ref() {
            let dpi = GetDpiForSystem() as i32;
            let scale = |v: i32| v * dpi / 96;
            let quiet = 4i32;
            let cell = (scale(QR_SIZE) / (c.width as i32 + quiet * 2)).max(1);
            let side = cell * (c.width as i32 + quiet * 2);
            let (x0, y0) = ((scale(QR_SIZE + MARGIN * 2) - side) / 2, scale(MARGIN));
            let black = CreateSolidBrush(COLORREF(0));
            for y in 0..c.width {
                for x in 0..c.width {
                    if c.modules[y * c.width + x] {
                        let (l, t) = (x0 + (x as i32 + quiet) * cell, y0 + (y as i32 + quiet) * cell);
                        FillRect(dc, &RECT { left: l, top: t, right: l + cell, bottom: t + cell }, black);
                    }
                }
            }
            let _ = DeleteObject(black);
            let top = y0 + side;
            let width = scale(QR_SIZE + MARGIN * 2);
            let font = |height: i32, weight: i32| {
                CreateFontW(
                    -scale(height), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32,
                    CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32, 0, w!("Segoe UI"),
                )
            };
            let line = |text: &str, f: HFONT, y: i32, height: i32| {
                let old = SelectObject(dc, f);
                let mut r = RECT { left: 0, top: top + scale(y), right: width, bottom: top + scale(y + height) };
                let mut wide: Vec<u16> = text.encode_utf16().collect();
                DrawTextW(dc, &mut wide, &mut r, DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
                SelectObject(dc, old);
                let _ = DeleteObject(f);
            };
            let _ = SetBkMode(dc, TRANSPARENT);
            line(&format!("On your iPad, open Sill, tap {}, and scan this.", c.name), font(18, 400), 4, 30);
            line("Or tap Enter Code Instead and type:", font(16, 400), 40, 26);
            line(&crate::pairing::grouped(&c.digits), font(34, 600), 66, 48);
            line("This code works once. A new one appears after 5 minutes.", font(13, 400), 116, 22);
        }
        drop(guard);
        let _ = EndPaint(h, &ps);
    }

    unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match m {
            WM_PAINT => {
                paint(h);
                LRESULT(0)
            }
            WM_CLOSE | HIDE => {
                let _ = ShowWindow(h, SW_HIDE);
                LRESULT(0)
            }
            // Shown without taking the keyboard: a device's ask must never swallow what someone is typing.
            SHOW => {
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                LRESULT(0)
            }
            _ => DefWindowProcW(h, m, w, l),
        }
    }

    /// Starts the window's thread. Returns false if it couldn't be made (the console's code remains).
    pub fn start(show: bool) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("pair window".into())
            .spawn(move || unsafe {
                let Ok(module) = GetModuleHandleW(PCWSTR::null()) else { return };
                let instance: HINSTANCE = module.into();
                let class = w!("SillPairWindow");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    hInstance: instance,
                    lpszClassName: class,
                    hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                if RegisterClassW(&wc) == 0 {
                    let _ = tx.send(false);
                    return;
                }
                let dpi = GetDpiForSystem() as i32;
                let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
                let mut r = RECT { left: 0, top: 0, right: (QR_SIZE + MARGIN * 2) * dpi / 96, bottom: (QR_SIZE + MARGIN * 2 + TEXT) * dpi / 96 };
                let _ = AdjustWindowRect(&mut r, style, false);
                let Ok(h) = CreateWindowExW(
                    WINDOW_EX_STYLE(0), class, w!("Pair with Sill"), style, CW_USEDEFAULT, CW_USEDEFAULT,
                    r.right - r.left, r.bottom - r.top, None, None, instance, None,
                ) else {
                    let _ = tx.send(false);
                    return;
                };
                WINDOW.store(h.0 as isize, Ordering::SeqCst);
                if show {
                    let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                }
                let _ = tx.send(true);
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            })
            .is_ok()
            && rx.recv().unwrap_or(false)
    }
}

#[cfg(windows)]
pub use imp::{bring_up, hide, set, start};

// Outside Windows there's only the console (development and tests).
#[cfg(not(windows))]
pub fn set(_: Code) {}
#[cfg(not(windows))]
pub fn bring_up() {}
#[cfg(not(windows))]
pub fn hide() {}
#[cfg(not(windows))]
pub fn start(_: bool) -> bool {
    false
}
