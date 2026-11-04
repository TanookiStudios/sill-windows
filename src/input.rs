//! The device's input (kind 8) as Windows input, through SendInput.
//!
//! Mac → Windows keys (the one table; README.md repeats it in plain words):
//!
//! | On the iPad keyboard      | On Windows                                   |
//! |---------------------------|----------------------------------------------|
//! | Command (⌘)               | Ctrl                                         |
//! | Control (⌃)               | Ctrl                                         |
//! | Option (⌥)                | Alt                                          |
//! | Shift (⇧)                 | Shift                                        |
//! | Command-Space             | the Windows key (Start and search)           |
//! | Delete (⌫)                | Backspace                                    |
//! | Forward Delete (⌦)        | Delete                                       |
//! | Return, Tab, Esc, arrows, F1–F24, Home/End/Page Up/Page Down, number pad | the same keys |
//!
//! So ⌘C, ⌘V, ⌘Z, ⌘S, ⌘A, ⌘F and ⌘W do what Ctrl+C… do on Windows, and ⌥Tab is Alt+Tab.
//! Typed text arrives as Unicode characters and is typed as they are, whatever the keyboard layout.
//!
//! Pointer positions are fractions of the streamed picture, mapped onto the captured monitor's
//! rectangle on the virtual desktop. Scrolls are fractions of the picture too, turned into wheel
//! units (120 per notch, about 50 pixels).

use crate::protocol::{InputEvent, PointerAction};

/// The captured monitor on the virtual desktop, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

pub const SHIFT: u64 = 1 << 17;
pub const CONTROL: u64 = 1 << 18;
pub const OPTION: u64 = 1 << 19;
pub const COMMAND: u64 = 1 << 20;

const VK_LSHIFT: u16 = 0xA0;
const VK_LCONTROL: u16 = 0xA2;
const VK_LMENU: u16 = 0xA4;
pub const VK_LWIN: u16 = 0x5B;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_TAB: u16 = 0x09;
pub const VK_BACK: u16 = 0x08;
/// Pixels of scroll per wheel notch (120 units).
const PIXELS_PER_NOTCH: f64 = 50.0;

/// A USB HID usage (keyboard page) as a Windows virtual key, and whether it is an "extended" key.
pub fn virtual_key(usage: u16) -> Option<(u16, bool)> {
    let vk = match usage {
        0x04..=0x1D => 0x41 + (usage - 0x04),            // A–Z
        0x1E..=0x26 => 0x31 + (usage - 0x1E),            // 1–9
        0x27 => 0x30,                                    // 0
        0x28 => VK_RETURN,
        0x29 => 0x1B,                                    // Escape
        0x2A => VK_BACK,                                 // Delete (backspace)
        0x2B => VK_TAB,
        0x2C => 0x20,                                    // Space
        0x2D => 0xBD, 0x2E => 0xBB, 0x2F => 0xDB, 0x30 => 0xDD, 0x31 => 0xDC, 0x32 => 0xDC,
        0x33 => 0xBA, 0x34 => 0xDE, 0x35 => 0xC0, 0x36 => 0xBC, 0x37 => 0xBE, 0x38 => 0xBF,
        0x39 => 0x14,                                    // Caps Lock
        0x3A..=0x45 => 0x70 + (usage - 0x3A),            // F1–F12
        0x46 => 0x2C, 0x47 => 0x91, 0x48 => 0x13,        // Print Screen, Scroll Lock, Pause
        0x49 => return Some((0x2D, true)),               // Insert
        0x4A => return Some((0x24, true)),               // Home
        0x4B => return Some((0x21, true)),               // Page Up
        0x4C => return Some((0x2E, true)),               // Forward Delete
        0x4D => return Some((0x23, true)),               // End
        0x4E => return Some((0x22, true)),               // Page Down
        0x4F => return Some((0x27, true)),               // Right
        0x50 => return Some((0x25, true)),               // Left
        0x51 => return Some((0x28, true)),               // Down
        0x52 => return Some((0x26, true)),               // Up
        0x53 => 0x90,                                    // Num Lock
        0x54 => return Some((0x6F, true)),               // keypad /
        0x55 => 0x6A, 0x56 => 0x6D, 0x57 => 0x6B,        // keypad * - +
        0x58 => return Some((VK_RETURN, true)),          // keypad Enter
        0x59..=0x61 => 0x61 + (usage - 0x59),            // keypad 1–9
        0x62 => 0x60, 0x63 => 0x6E,                      // keypad 0 .
        0x64 => 0xE2,                                    // the key beside left Shift on ISO keyboards
        0x65 => return Some((0x5D, true)),               // Application (menu)
        0x68..=0x73 => 0x7C + (usage - 0x68),            // F13–F24
        0x7F => 0xAD, 0x80 => 0xAF, 0x81 => 0xAE,        // mute, volume up, volume down
        0xE0 | 0xE3 => VK_LCONTROL,                      // left Control, left Command
        0xE1 => VK_LSHIFT,
        0xE2 => VK_LMENU,                                // left Option
        0xE4 | 0xE7 => return Some((0xA3, true)),        // right Control, right Command
        0xE5 => 0xA1,
        0xE6 => return Some((0xA5, true)),               // right Option
        _ => return None,
    };
    Some((vk, false))
}

fn is_modifier_usage(usage: u16) -> bool {
    (0xE0..=0xE7).contains(&usage)
}

/// The modifier keys a device's flags ask for, as left-hand virtual keys (⌘ and ⌃ are both Ctrl).
pub fn modifier_keys(flags: u64) -> Vec<u16> {
    let mut v = Vec::new();
    if flags & (COMMAND | CONTROL) != 0 {
        v.push(VK_LCONTROL);
    }
    if flags & OPTION != 0 {
        v.push(VK_LMENU);
    }
    if flags & SHIFT != 0 {
        v.push(VK_LSHIFT);
    }
    v
}

/// One key going down or up: (virtual key, extended, down).
pub type Stroke = (u16, bool, bool);

/// Turns the device's keys into strokes, keeping track of which modifiers it holds and which this
/// host pressed for a shortcut, so every down gets its up and nothing is left held.
#[derive(Default)]
pub struct Keys {
    /// Modifier keys the device pressed itself (a hardware keyboard, or a latched modifier).
    held: Vec<u16>,
    /// For each key down: the modifiers pressed for it, let go again with its up.
    added: std::collections::HashMap<u16, Vec<u16>>,
}

impl Keys {
    pub fn key(&mut self, usage: u16, down: bool, flags: u64) -> Vec<Stroke> {
        let Some((vk, ext)) = virtual_key(usage) else { return vec![] };
        if is_modifier_usage(usage) {
            if down && !self.held.contains(&vk) {
                self.held.push(vk);
            } else if !down {
                self.held.retain(|&h| h != vk);
            }
            return vec![(vk, ext, down)];
        }
        // ⌘Space alone: the Windows key, which opens Start and its search (Spotlight's place).
        if usage == 0x2C && flags & (COMMAND | CONTROL | OPTION | SHIFT) == COMMAND {
            if !down {
                return vec![];
            }
            let mut s: Vec<Stroke> = self.held.iter().rev().map(|&h| (h, false, false)).collect();
            self.held.clear();
            s.extend([(VK_LWIN, true, true), (VK_LWIN, true, false)]);
            return s;
        }
        if down {
            let extra: Vec<u16> = modifier_keys(flags).into_iter().filter(|m| !self.held.contains(m)).collect();
            let mut s: Vec<Stroke> = extra.iter().map(|&m| (m, false, true)).collect();
            s.push((vk, ext, true));
            if let Some(old) = self.added.insert(usage, extra) {
                s.extend(old.into_iter().rev().map(|m| (m, false, false)));
            }
            s
        } else {
            let mut s = vec![(vk, ext, false)];
            if let Some(extra) = self.added.remove(&usage) {
                s.extend(extra.into_iter().rev().map(|m| (m, false, false)));
            }
            s
        }
    }

    /// Lets go of everything still held (before typed text, and when a device leaves).
    pub fn release_all(&mut self) -> Vec<Stroke> {
        let mut s: Vec<Stroke> = Vec::new();
        for (usage, extra) in self.added.drain() {
            if let Some((vk, ext)) = virtual_key(usage) {
                s.push((vk, ext, false));
            }
            s.extend(extra.into_iter().map(|m| (m, false, false)));
        }
        s.extend(self.held.drain(..).map(|h| (h, false, false)));
        s
    }
}

/// Scroll fractions as wheel units, keeping the remainder so slow drags still scroll.
#[derive(Default)]
pub struct Wheel {
    rest_x: f64,
    rest_y: f64,
}

impl Wheel {
    /// (vertical, horizontal) wheel units. Positive dy (content moves down) is a forward wheel turn;
    /// positive dx (content moves right) scrolls left, which is a negative horizontal wheel.
    pub fn units(&mut self, dx: f64, dy: f64, rect: Rect) -> (i32, i32) {
        let y = dy * rect.height as f64 * 120.0 / PIXELS_PER_NOTCH + self.rest_y;
        let x = -dx * rect.width as f64 * 120.0 / PIXELS_PER_NOTCH + self.rest_x;
        let (vy, vx) = (y.trunc(), x.trunc());
        self.rest_y = y - vy;
        self.rest_x = x - vx;
        (vy as i32, vx as i32)
    }
}

/// What the injector does with each event, as plain values (so the logic is tested anywhere).
#[derive(Debug, PartialEq)]
pub enum Action {
    MoveTo(i32, i32),
    Button { right: bool, down: bool },
    Wheel { vertical: i32, horizontal: i32 },
    Keys(Vec<Stroke>),
    Unicode(Vec<u16>),
}

#[derive(Default)]
pub struct Translator {
    pub keys: Keys,
    wheel: Wheel,
}

impl Translator {
    pub fn translate(&mut self, event: &InputEvent, rect: Rect) -> Vec<Action> {
        let point = |x: f64, y: f64| {
            let px = rect.left + (x.clamp(0.0, 1.0) * (rect.width - 1).max(0) as f64).round() as i32;
            let py = rect.top + (y.clamp(0.0, 1.0) * (rect.height - 1).max(0) as f64).round() as i32;
            Action::MoveTo(px, py)
        };
        match event {
            InputEvent::Pointer { action, x, y } => {
                let mut a = vec![point(*x, *y)];
                match action {
                    PointerAction::Move => {}
                    PointerAction::LeftDown => a.push(Action::Button { right: false, down: true }),
                    PointerAction::LeftUp => a.push(Action::Button { right: false, down: false }),
                    PointerAction::RightDown => a.push(Action::Button { right: true, down: true }),
                    PointerAction::RightUp => a.push(Action::Button { right: true, down: false }),
                }
                a
            }
            InputEvent::Scroll { x, y, dx, dy } => {
                let (vertical, horizontal) = self.wheel.units(*dx, *dy, rect);
                let mut a = vec![point(*x, *y)];
                if vertical != 0 || horizontal != 0 {
                    a.push(Action::Wheel { vertical, horizontal });
                }
                a
            }
            // Windows has no gesture phases for a wheel; the device makes its own momentum deltas.
            InputEvent::ScrollGesture { .. } => vec![],
            InputEvent::Key { hid_usage, down, modifiers } => {
                let s = self.keys.key(*hid_usage, *down, *modifiers);
                if s.is_empty() { vec![] } else { vec![Action::Keys(s)] }
            }
            InputEvent::Text { text } => {
                let mut a = Vec::new();
                let held = self.keys.release_all();
                if !held.is_empty() {
                    a.push(Action::Keys(held));
                }
                let mut run: Vec<u16> = Vec::new();
                for c in text.chars() {
                    let key = match c {
                        '\n' | '\r' => Some(VK_RETURN),
                        '\t' => Some(VK_TAB),
                        '\u{8}' => Some(VK_BACK),
                        _ => None,
                    };
                    match key {
                        Some(vk) => {
                            if !run.is_empty() {
                                a.push(Action::Unicode(std::mem::take(&mut run)));
                            }
                            a.push(Action::Keys(vec![(vk, false, true), (vk, false, false)]));
                        }
                        None => run.extend(c.encode_utf16(&mut [0; 2]).iter()),
                    }
                }
                if !run.is_empty() {
                    a.push(Action::Unicode(run));
                }
                a
            }
        }
    }
}

/// Runs the injector on its own thread: events in order, never blocking the network.
pub fn start(rect: std::sync::Arc<std::sync::Mutex<Rect>>) -> tokio::sync::mpsc::UnboundedSender<InputEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<InputEvent>();
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            let mut t = Translator::default();
            while let Some(e) = rx.blocking_recv() {
                let r = *rect.lock().unwrap();
                for a in t.translate(&e, r) {
                    post(a);
                }
            }
            for a in [Action::Keys(t.keys.release_all())] {
                post(a);
            }
        })
        .expect("input thread");
    tx
}

#[cfg(windows)]
fn post(action: Action) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};
    let mouse = |dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    let key = |vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    let inputs: Vec<INPUT> = unsafe {
        match action {
            Action::MoveTo(x, y) => {
                // Absolute positions are 0…65535 across the whole virtual desktop.
                let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
                let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2), GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2));
                let nx = ((x - vx) as i64 * 65535 / (vw - 1) as i64) as i32;
                let ny = ((y - vy) as i64 * 65535 / (vh - 1) as i64) as i32;
                vec![mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK)]
            }
            Action::Button { right, down } => {
                let f = match (right, down) {
                    (false, true) => MOUSEEVENTF_LEFTDOWN,
                    (false, false) => MOUSEEVENTF_LEFTUP,
                    (true, true) => MOUSEEVENTF_RIGHTDOWN,
                    (true, false) => MOUSEEVENTF_RIGHTUP,
                };
                vec![mouse(0, 0, 0, f)]
            }
            Action::Wheel { vertical, horizontal } => {
                let mut v = Vec::new();
                if vertical != 0 {
                    v.push(mouse(0, 0, vertical, MOUSEEVENTF_WHEEL));
                }
                if horizontal != 0 {
                    v.push(mouse(0, 0, horizontal, MOUSEEVENTF_HWHEEL));
                }
                v
            }
            Action::Keys(strokes) => strokes
                .into_iter()
                .map(|(vk, ext, down)| {
                    let scan = MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) as u16;
                    let mut f = if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP };
                    if ext {
                        f |= KEYEVENTF_EXTENDEDKEY;
                    }
                    key(vk, scan, f)
                })
                .collect(),
            Action::Unicode(units) => units
                .into_iter()
                .flat_map(|u| [key(0, u, KEYEVENTF_UNICODE), key(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)])
                .collect(),
        }
    };
    if !inputs.is_empty() {
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            crate::warn!("Windows took {sent} of {} input events (another program's window may be running as administrator).", inputs.len());
        }
    }
}

#[cfg(not(windows))]
fn post(action: Action) {
    if std::env::var_os("SILL_LOG_INPUT").is_some() {
        crate::say!("input: {action:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const R: Rect = Rect { left: 0, top: 0, width: 1920, height: 1080 };

    #[test]
    fn command_c_is_ctrl_c() {
        let mut k = Keys::default();
        assert_eq!(k.key(0x06, true, COMMAND), vec![(VK_LCONTROL, false, true), (0x43, false, true)]);
        assert_eq!(k.key(0x06, false, 0), vec![(0x43, false, false), (VK_LCONTROL, false, false)]);
        assert!(k.release_all().is_empty(), "nothing left held");
    }

    #[test]
    fn hardware_modifier_is_not_pressed_twice() {
        let mut k = Keys::default();
        assert_eq!(k.key(0xE3, true, COMMAND), vec![(VK_LCONTROL, false, true)]);
        assert_eq!(k.key(0x19, true, COMMAND), vec![(0x56, false, true)], "⌘ already down: just V");
        assert_eq!(k.key(0x19, false, COMMAND), vec![(0x56, false, false)]);
        assert_eq!(k.key(0xE3, false, 0), vec![(VK_LCONTROL, false, false)]);
        assert!(k.release_all().is_empty());
    }

    #[test]
    fn command_space_is_the_windows_key() {
        let mut k = Keys::default();
        assert_eq!(k.key(0x2C, true, COMMAND), vec![(VK_LWIN, true, true), (VK_LWIN, true, false)]);
        assert!(k.key(0x2C, false, COMMAND).is_empty());
        assert_eq!(k.key(0x2C, true, 0), vec![(0x20, false, true)], "plain Space types a space");
    }

    #[test]
    fn arrows_are_extended() {
        assert_eq!(virtual_key(0x50), Some((0x25, true)));
        assert_eq!(virtual_key(0x04), Some((0x41, false)));
        assert_eq!(virtual_key(0x27), Some((0x30, false)));
        assert_eq!(virtual_key(0x45), Some((0x7B, false)), "F12");
        assert_eq!(virtual_key(0x02), None);
    }

    #[test]
    fn pointer_and_scroll() {
        let mut t = Translator::default();
        let r = Rect { left: -1920, top: 0, width: 1920, height: 1080 };
        let e = InputEvent::Pointer { action: PointerAction::LeftDown, x: 1.0, y: 0.5 };
        assert_eq!(t.translate(&e, r), vec![Action::MoveTo(-1, 540), Action::Button { right: false, down: true }]);
        // Content dragged down by 50 px: one notch forward.
        let s = InputEvent::Scroll { x: 0.5, y: 0.5, dx: 0.0, dy: 50.0 / 1080.0 };
        assert_eq!(t.translate(&s, R)[1], Action::Wheel { vertical: 120, horizontal: 0 });
        // Tiny steps add up instead of being lost.
        let tiny = InputEvent::Scroll { x: 0.5, y: 0.5, dx: 0.0, dy: 0.1 / 1080.0 };
        let total: i32 = (0..1000)
            .flat_map(|_| t.translate(&tiny, R))
            .map(|a| if let Action::Wheel { vertical, .. } = a { vertical } else { 0 })
            .sum();
        assert!((239..=240).contains(&total), "{total}");
    }

    #[test]
    fn text_with_return_and_backspace() {
        let mut t = Translator::default();
        let a = t.translate(&InputEvent::Text { text: "Hé\n\u{8}😀".into() }, R);
        assert_eq!(a, vec![
            Action::Unicode(vec![0x48, 0xE9]),
            Action::Keys(vec![(VK_RETURN, false, true), (VK_RETURN, false, false)]),
            Action::Keys(vec![(VK_BACK, false, true), (VK_BACK, false, false)]),
            Action::Unicode(vec![0xD83D, 0xDE00]),
        ]);
    }
}
