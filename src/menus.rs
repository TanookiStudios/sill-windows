//! The PC's menus on the device (kinds 24, 25 and 27, MacMenu.swift): the streamed window's menu bar
//! (the Desktop's: the window in front), read through Windows' own menu API when the device opens a
//! menu, and an item chosen there sent to the window as its own menu would send it. Only classic
//! Windows menus (an HMENU) can be read this way. Apps that draw their own, which is most modern
//! ones, show none, with a note that says so.

use crate::protocol as p;

pub const CHANGED: &str = "The menus changed. Open the menu again.";
pub const NONE: &str = "This app’s menus can’t be shown. Sill can only read classic Windows menus.";
pub const LOCKED: &str = "This PC is locked.";
/// The most items one answer carries (MacMenu.swift).
pub const MAX_ITEMS: usize = 500;

/// One menu or item, as Windows holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub title: String,
    pub key: Option<String>,
    pub enabled: bool,
    pub mark: bool,
    pub separator: bool,
    pub submenu: bool,
}

/// One window's menu bar, as last read.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub window: u32,
    pub app: String,
    pub path: String,
    pub top: Vec<Node>,
    pub stale: bool,
}

/// The tree the devices see: its version, and the bar it was read from (None: no menus).
#[derive(Debug, Default)]
pub struct State {
    pub version: i64,
    pub bar: Option<Bar>,
}

impl State {
    /// Takes a fresh read; the version moves when the window, the app or the top level changed.
    pub fn update(&mut self, bar: Option<Bar>) -> bool {
        let key = |b: &Option<Bar>| b.as_ref().map(|b| (b.window, b.app.clone(), b.top.clone()));
        let changed = key(&self.bar) != key(&bar) || self.version == 0;
        if changed {
            self.version += 1;
        }
        self.bar = bar;
        changed
    }

    /// Kind 24's top level.
    pub fn top_level(&self, answering: Option<i64>) -> p::MacMenu {
        let mut m = p::MacMenu { version: Some(self.version), answering, menus: Some(vec![]), ..Default::default() };
        match &self.bar {
            Some(b) => {
                m.app = Some(b.app.clone());
                m.bundle_id = Some(b.path.clone());
                m.menus = Some(items(&[], &b.top).0);
                if b.stale {
                    m.stale = Some(true);
                    m.note = Some(format!("{} isn’t responding.", b.app));
                }
            }
            None => m.note = Some(NONE.into()),
        }
        m
    }

    /// The bar a fetch or a press of this version may act on: None answers "The menus changed".
    pub fn current(&self, version: Option<i64>) -> Option<&Bar> {
        self.bar.as_ref().filter(|_| version == Some(self.version))
    }
}

/// An answer that only carries words.
pub fn refusal(version: i64, answering: Option<i64>, menu: Option<String>, pressed: Option<bool>, note: &str) -> p::MacMenu {
    p::MacMenu { version: Some(version), answering, menu, pressed, note: Some(note.into()), ..Default::default() }
}

/// "&Save\tCtrl+S": the title Windows draws ("Save") and its shortcut as a Mac draws it ("⌘S"),
/// since the iPad's ⌘ arrives as Ctrl. "..." becomes the "…" Sill's menus use.
pub fn label(raw: &str) -> (String, Option<String>) {
    let (text, key) = match raw.split_once('\t') {
        Some((t, k)) => (t, Some(k.trim())),
        None => (raw, None),
    };
    let mut title = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            // "&&" is a real ampersand; one alone marks the access key.
            if chars.peek() == Some(&'&') {
                chars.next();
                title.push('&');
            }
            continue;
        }
        title.push(c);
    }
    let title = title.trim().replace("...", "…");
    (title, key.filter(|k| !k.is_empty()).map(mac_key))
}

/// "Ctrl+Shift+S" to "⇧⌘S", in a Mac's order of modifiers (⌃⌥⇧⌘). Anything it doesn't know stays
/// as Windows wrote it.
pub fn mac_key(windows: &str) -> String {
    // "Ctrl++": the key itself is the plus.
    let (body, plus) = match windows.strip_suffix("++") {
        Some(b) => (b, true),
        None => (windows, false),
    };
    let mut parts: Vec<&str> = body.split('+').map(str::trim).collect();
    if plus {
        parts.push("+");
    }
    let (Some((last, mods)), true) = (parts.split_last(), parts.len() > 1) else { return windows.to_string() };
    let (mut alt, mut shift, mut cmd) = (false, false, false);
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => cmd = true,
            "shift" => shift = true,
            "alt" => alt = true,
            _ => return windows.to_string(),
        }
    }
    format!("{}{}{}{}", if alt { "⌥" } else { "" }, if shift { "⇧" } else { "" }, if cmd { "⌘" } else { "" }, last)
}

/// "2.9" to [2, 9]: indexes from the menu bar, as Windows counts them (separators included).
pub fn parse_id(id: &str) -> Option<Vec<usize>> {
    let path: Vec<usize> = id.split('.').map(|s| s.parse().ok().filter(|&i: &usize| i < 4096)).collect::<Option<_>>()?;
    (!path.is_empty() && path.len() <= 12).then_some(path)
}

/// One menu's nodes as the device draws them, each id its path; at most `MAX_ITEMS`, and how many
/// more there were. A picture-only item (no text) is left out, its index still counted.
pub fn items(parent: &[usize], nodes: &[Node]) -> (Vec<p::MacMenuItem>, usize) {
    let mut out = Vec::new();
    let mut more = 0;
    for (i, n) in nodes.iter().enumerate() {
        if !n.separator && n.title.is_empty() {
            continue;
        }
        if out.len() == MAX_ITEMS {
            more += 1;
            continue;
        }
        out.push(if n.separator {
            p::MacMenuItem { separator: Some(true), ..Default::default() }
        } else {
            let id = parent.iter().chain([&i]).map(|i| i.to_string()).collect::<Vec<_>>().join(".");
            p::MacMenuItem {
                id: Some(id),
                title: Some(n.title.clone()),
                enabled: (!n.enabled).then_some(false),
                mark: n.mark.then(|| "✓".to_string()),
                key: n.key.clone(),
                submenu: n.submenu.then_some(true),
                ..Default::default()
            }
        });
    }
    (out, more)
}

/// Why a read or a press didn't happen, in the device's words.
pub type Refused = String;

#[cfg(windows)]
pub mod platform {
    use super::{label, Bar, Node, Refused, CHANGED};
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    fn hwnd(id: u32) -> HWND {
        HWND(id as usize as *mut _)
    }

    /// The window in front, unless it is one of this program's own.
    pub fn foreground() -> Option<u32> {
        unsafe {
            let h = GetForegroundWindow();
            if h.0.is_null() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            (pid != std::process::id()).then_some(h.0 as usize as u32)
        }
    }

    unsafe fn read(menu: HMENU) -> Vec<(Node, u32, HMENU)> {
        let count = GetMenuItemCount(menu);
        let mut out = Vec::new();
        for i in 0..count.clamp(0, 4096) as u32 {
            let mut info = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU | MIIM_STRING,
                ..Default::default()
            };
            if GetMenuItemInfoW(menu, i, true, &mut info).is_err() {
                out.push((Node { title: String::new(), key: None, enabled: false, mark: false, separator: false, submenu: false }, 0, HMENU::default()));
                continue;
            }
            let separator = info.fType.0 & MFT_SEPARATOR.0 != 0;
            let mut text = String::new();
            if !separator && info.cch > 0 {
                let mut b = vec![0u16; info.cch as usize + 1];
                info.dwTypeData = windows::core::PWSTR(b.as_mut_ptr());
                info.cch += 1;
                info.fMask = MIIM_STRING;
                if GetMenuItemInfoW(menu, i, true, &mut info).is_ok() {
                    text = String::from_utf16_lossy(&b[..info.cch as usize]);
                }
            }
            let (title, key) = label(&text);
            let state = info.fState.0;
            out.push((
                Node {
                    title,
                    key,
                    enabled: state & (MFS_DISABLED.0 | MFS_GRAYED.0) == 0,
                    mark: state & MFS_CHECKED.0 != 0,
                    separator,
                    submenu: !info.hSubMenu.0.is_null(),
                },
                info.wID,
                info.hSubMenu,
            ));
        }
        out
    }

    unsafe fn hung(h: HWND) -> bool {
        IsHungAppWindow(h).as_bool()
    }

    /// The window's menu bar; None when it has none Windows can read.
    pub fn bar(window: u32) -> Option<Bar> {
        unsafe {
            let h = hwnd(window);
            if !IsWindow(h).as_bool() {
                return None;
            }
            let menu = GetMenu(h);
            if menu.0.is_null() || !IsMenu(menu).as_bool() {
                return None;
            }
            let top: Vec<Node> = read(menu).into_iter().map(|(n, _, _)| n).collect();
            if top.iter().all(|n| n.title.is_empty()) {
                return None;
            }
            let path = crate::catalog::platform::program_path(window).unwrap_or_default();
            let app = crate::catalog::platform::name_of(&path);
            Some(Bar { window, app, path, top, stale: hung(h) })
        }
    }

    /// Walks to the item at `path`, which must still be titled `title`: its parent, index, ID and node.
    unsafe fn find(window: u32, path: &[usize], title: &str) -> Result<(HMENU, usize, u32, Node, HMENU), Refused> {
        let mut menu = GetMenu(hwnd(window));
        if menu.0.is_null() {
            return Err(CHANGED.into());
        }
        for (depth, &index) in path.iter().enumerate() {
            let nodes = read(menu);
            let Some((node, id, sub)) = nodes.get(index).cloned() else { return Err(CHANGED.into()) };
            if depth + 1 == path.len() {
                if node.title != title {
                    return Err(CHANGED.into());
                }
                return Ok((menu, index, id, node, sub));
            }
            if sub.0.is_null() {
                return Err(CHANGED.into());
            }
            // The app fills or updates a menu as it opens.
            let _ = SendMessageTimeoutW(hwnd(window), WM_INITMENUPOPUP, WPARAM(sub.0 as usize), LPARAM(index as isize), SMTO_ABORTIFHUNG | SMTO_BLOCK, 500, None);
            menu = sub;
        }
        Err(CHANGED.into())
    }

    /// One submenu's items, as fresh as the app makes them when it opens.
    pub fn items(window: u32, path: &[usize], title: &str) -> Result<Vec<Node>, Refused> {
        unsafe {
            let h = hwnd(window);
            if hung(h) {
                return Err("The app isn’t responding.".into());
            }
            let (_, index, _, node, sub) = find(window, path, title)?;
            if !node.submenu || sub.0.is_null() {
                return Err(CHANGED.into());
            }
            let r = SendMessageTimeoutW(h, WM_INITMENUPOPUP, WPARAM(sub.0 as usize), LPARAM(index as isize), SMTO_ABORTIFHUNG | SMTO_BLOCK, 1000, None);
            let nodes = read(sub).into_iter().map(|(n, _, _)| n).collect();
            let _ = SendMessageTimeoutW(h, WM_UNINITMENUPOPUP, WPARAM(sub.0 as usize), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, 500, None);
            if r.0 == 0 {
                return Err("The app isn’t responding.".into());
            }
            Ok(nodes)
        }
    }

    /// Chooses an item as Windows' own menu does: WM_COMMAND with its ID (WM_MENUCOMMAND for a
    /// menu that asks for positions).
    pub fn press(window: u32, path: &[usize], title: &str) -> Result<(), Refused> {
        unsafe {
            let h = hwnd(window);
            let (parent, index, id, node, _) = find(window, path, title)?;
            if node.separator || node.submenu {
                return Err(CHANGED.into());
            }
            if !node.enabled {
                return Err(format!("“{}” is dimmed right now.", node.title));
            }
            let mut info = MENUINFO { cbSize: std::mem::size_of::<MENUINFO>() as u32, fMask: MIM_STYLE, ..Default::default() };
            let by_position = GetMenuInfo(parent, &mut info).is_ok() && info.dwStyle.0 & MNS_NOTIFYBYPOS.0 != 0;
            let sent = if by_position {
                PostMessageW(h, WM_MENUCOMMAND, WPARAM(index), LPARAM(parent.0 as isize))
            } else {
                PostMessageW(h, WM_COMMAND, WPARAM((id & 0xFFFF) as usize), LPARAM(0))
            };
            sent.map_err(|_| "Windows didn’t let Sill choose that. The app may be running as administrator.".to_string())
        }
    }
}

/// Off Windows: one window with a small menu bar while SILL_TEST_MENUS is set, so the tests reach
/// every answer.
#[cfg(not(windows))]
pub mod platform {
    use super::{Bar, Node, Refused, CHANGED};

    fn node(title: &str, key: Option<&str>, enabled: bool, mark: bool, submenu: bool) -> Node {
        Node { title: title.into(), key: key.map(Into::into), enabled, mark, separator: false, submenu }
    }
    fn tree(path: &[usize]) -> Option<Vec<Node>> {
        let sep = Node { title: String::new(), key: None, enabled: true, mark: false, separator: true, submenu: false };
        match path {
            [] => Some(vec![node("File", None, true, false, true), node("Edit", None, true, false, true)]),
            [0] => Some(vec![node("Save", Some("⌘S"), true, false, false), sep, node("Print…", None, false, false, false), node("Autosave", None, true, true, false)]),
            [1] => Some(vec![node("More", None, true, false, true)]),
            [1, 0] => Some(vec![node("Deep", None, true, false, false)]),
            _ => None,
        }
    }
    fn on() -> bool {
        std::env::var_os("SILL_TEST_MENUS").is_some()
    }
    pub fn foreground() -> Option<u32> {
        on().then_some(101)
    }
    pub fn bar(window: u32) -> Option<Bar> {
        (on() && window == 101).then(|| Bar { window, app: "Word".into(), path: "C:\\apps\\word.exe".into(), top: tree(&[]).unwrap(), stale: false })
    }
    fn find(path: &[usize], title: &str) -> Result<Node, Refused> {
        let (last, parent) = path.split_last().ok_or(CHANGED)?;
        let n = tree(parent).and_then(|t| t.get(*last).cloned()).ok_or(CHANGED)?;
        if n.title == title { Ok(n) } else { Err(CHANGED.into()) }
    }
    pub fn items(_: u32, path: &[usize], title: &str) -> Result<Vec<Node>, Refused> {
        find(path, title)?;
        tree(path).ok_or_else(|| CHANGED.into())
    }
    pub fn press(_: u32, path: &[usize], title: &str) -> Result<(), Refused> {
        let n = find(path, title)?;
        if n.submenu {
            return Err(CHANGED.into());
        }
        if !n.enabled {
            return Err(format!("“{}” is dimmed right now.", n.title));
        }
        crate::say!("(test) menu item {title} chosen");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_keys() {
        assert_eq!(label("&Save\tCtrl+S"), ("Save".into(), Some("⌘S".into())));
        assert_eq!(label("Save &As...\tCtrl+Shift+S"), ("Save As…".into(), Some("⇧⌘S".into())));
        assert_eq!(label("Fish && &Chips"), ("Fish & Chips".into(), None));
        assert_eq!(label("Zoom &In\tCtrl++"), ("Zoom In".into(), Some("⌘+".into())));
        assert_eq!(mac_key("F5"), "F5");
        assert_eq!(mac_key("Alt+F4"), "⌥F4");
        assert_eq!(mac_key("Win+E"), "Win+E");
    }

    #[test]
    fn ids() {
        assert_eq!(parse_id("2.9"), Some(vec![2, 9]));
        assert_eq!(parse_id("0"), Some(vec![0]));
        for bad in ["", "1.", "a", "-1", "99999", "1.2.3.4.5.6.7.8.9.10.11.12.13"] {
            assert_eq!(parse_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn items_skip_pictures_and_keep_indexes() {
        let n = |t: &str| Node { title: t.into(), key: None, enabled: true, mark: false, separator: false, submenu: false };
        let sep = Node { separator: true, ..n("") };
        let (out, more) = items(&[2], &[n("New"), sep, n(""), Node { enabled: false, mark: true, ..n("Old") }]);
        assert_eq!(more, 0);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].id.as_deref(), Some("2.0"));
        assert_eq!(out[1].separator, Some(true));
        assert_eq!((out[2].id.as_deref(), out[2].enabled, out[2].mark.as_deref()), (Some("2.3"), Some(false), Some("✓")));
        let many: Vec<Node> = (0..MAX_ITEMS + 7).map(|i| n(&i.to_string())).collect();
        let (out, more) = items(&[], &many);
        assert_eq!((out.len(), more), (MAX_ITEMS, 7));
    }

    #[test]
    fn version_moves_only_on_change() {
        let bar = |w: u32, t: &str| Some(Bar { window: w, app: "A".into(), path: "a.exe".into(), top: vec![Node { title: t.into(), key: None, enabled: true, mark: false, separator: false, submenu: true }], stale: false });
        let mut s = State::default();
        assert!(s.update(None));
        assert_eq!(s.version, 1);
        assert!(!s.update(None));
        assert!(s.update(bar(5, "File")));
        assert!(!s.update(bar(5, "File")));
        assert!(s.update(bar(5, "Edit")));
        assert!(s.update(bar(6, "Edit")));
        assert_eq!(s.version, 4);
        assert!(s.current(Some(4)).is_some() && s.current(Some(3)).is_none() && s.current(None).is_none());
        assert_eq!(s.top_level(Some(9)).menus.unwrap()[0].title.as_deref(), Some("Edit"));
        s.update(None);
        assert_eq!(s.top_level(None).note.as_deref(), Some(NONE));
    }
}
