//! One log file, `Sill-log.txt` beside SillHost.exe (or in the Sill folder when that can't be
//! written): what the console says, with times, so a run can be read afterward. Never the pairing
//! code or its link. Each start moves the last run's log to `Sill-log-previous.txt`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn init() {
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from));
    for dir in beside.into_iter().chain([crate::identity::home()]) {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("Sill-log.txt");
        let _ = std::fs::rename(&path, dir.join("Sill-log-previous.txt"));
        if let Ok(f) = std::fs::File::create(&path) {
            *FILE.lock().unwrap() = Some(f);
            *PATH.lock().unwrap() = Some(path);
            break;
        }
    }
    std::panic::set_hook(Box::new(|info| {
        crate::warn!("Sill crashed: {info}");
    }));
}

pub fn path() -> Option<PathBuf> {
    PATH.lock().unwrap().clone()
}

pub fn file(line: &str) {
    if let Some(f) = FILE.lock().unwrap().as_mut() {
        let t = crate::protocol::now() as u64;
        let _ = writeln!(f, "{:02}:{:02}:{:02} UTC  {line}", t / 3600 % 24, t / 60 % 60, t % 60);
    }
}

/// Says it on the console and in the log.
#[macro_export]
macro_rules! say {
    ($($a:tt)*) => {{ let s = format!($($a)*); println!("{s}"); $crate::log::file(&s); }};
}

/// The same, for what went wrong.
#[macro_export]
macro_rules! warn {
    ($($a:tt)*) => {{ let s = format!($($a)*); eprintln!("{s}"); $crate::log::file(&s); }};
}
