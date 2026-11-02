//! Sill for Windows: lets the Sill app on an iPad or iPhone see and control this PC.
//! See README.md for what it does and doesn't do yet.

use sill_windows::{identity, input, mdns, pairing, protocol, server, video};

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

const USAGE: &str = "SillHost — lets Sill on an iPad or iPhone see and control this PC.

Usage: SillHost [options]
  --port N            the network port (default 7455)
  --bitrate MBPS      picture quality in megabits per second (default 15)
  --max-size PIXELS   the picture's longer side at most this many pixels (default 2560)
  --name NAME         the name devices show (default: this PC's name)
  --forget-devices    unpair every iPad and iPhone, then exit
  --help              this text";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(args) {
        eprintln!("\nSill couldn't start: {e}");
        pause_before_exit();
        std::process::exit(1);
    }
}

/// When someone double-clicked the program, keep its window open long enough to read why it stopped.
fn pause_before_exit() {
    #[cfg(windows)]
    {
        eprintln!("Press Enter to close this window.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

struct Options {
    port: u16,
    bitrate_mbps: u32,
    max_side: u32,
    name: Option<String>,
    forget: bool,
}

fn options(args: Vec<String>) -> Result<Options, String> {
    let mut o = Options { port: 7455, bitrate_mbps: 15, max_side: 2560, name: None, forget: false };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value = |what: &str| it.next().ok_or(format!("{a} needs {what}"));
        match a.as_str() {
            "--port" => o.port = value("a number")?.parse().map_err(|_| "--port needs a number from 1 to 65535")?,
            "--bitrate" => o.bitrate_mbps = value("a number")?.parse().ok().filter(|b| (1..=150).contains(b)).ok_or("--bitrate needs 1 to 150")?,
            "--max-size" => o.max_side = value("a number")?.parse().ok().filter(|s| (320..=8192).contains(s)).ok_or("--max-size needs 320 to 8192")?,
            "--name" => o.name = Some(protocol::safe_label(&value("a name")?, 64)).filter(|n| !n.is_empty()),
            "--forget-devices" => o.forget = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option {a}\n\n{USAGE}")),
        }
    }
    if o.port == 0 {
        return Err("--port needs a number from 1 to 65535".into());
    }
    Ok(o)
}

fn run(args: Vec<String>) -> Result<(), String> {
    let o = options(args)?;
    #[cfg(windows)]
    console_setup();
    let trust = identity::Trust::load()?;
    if o.forget {
        let n = trust.count();
        trust.clear()?;
        println!("Unpaired {n} device(s). Each one pairs again with a new code next time.");
        return Ok(());
    }
    let identity = identity::Identity::load_or_create()?;
    let name = o.name.clone().unwrap_or_else(pc_name);
    let rng = ring::rand::SystemRandom::new();

    println!("Sill for Windows {}", env!("CARGO_PKG_VERSION"));
    println!("This PC appears in Sill as \"{name}\" (PC ID {}).", pairing::mac_id(&identity.fingerprint));
    println!("Paired devices: {}.", trust.count());

    let hub = Arc::new(server::Hub::default());
    hub.bitrate.store(o.bitrate_mbps as u64 * 1_000_000, Ordering::SeqCst);
    let rect = Arc::new(Mutex::new(primary_rect()));
    let input = input::start(rect.clone());
    video::start(hub.clone(), video::Settings { fps: 60, max_side: o.max_side }, rect);

    let window = Arc::new(Mutex::new(new_window(&rng)));
    let launch_id = {
        let mut b = [0u8; 8];
        ring::rand::SecureRandom::fill(&rng, &mut b).map_err(|_| "no random source")?;
        pairing::b64u(&b)
    };
    let fingerprint = identity.fingerprint;
    let tag = {
        let mut nonce = [0u8; 6];
        ring::rand::SecureRandom::fill(&rng, &mut nonce).map_err(|_| "no random source")?;
        pairing::recognition_tag(&identity.recognition_key, nonce)
    };
    let host = Arc::new(server::Host::new(identity, trust, name.clone(), launch_id, hub.clone(), input, window.clone())?);

    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        server::listen(host.clone(), o.port).await?;
        let ipv4 = local_address().and_then(|a| a.parse().ok());
        let _advert = mdns::advertise(&name, o.port, &[("p", "1".to_string()), ("r", tag)], ipv4).map_err(|e| eprintln!("{e}")).ok();
        show_pairing(&window, &fingerprint, &name, o.port);
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = timer.tick() => {
                    let done = window.lock().unwrap().is_done(protocol::now());
                    if done {
                        *window.lock().unwrap() = new_window(&rng);
                        show_pairing(&window, &fingerprint, &name, o.port);
                    }
                }
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        println!("Quitting: telling connected devices.");
        server::goodbye_all(&hub).await;
        Ok::<(), String>(())
    })
}

fn new_window(rng: &ring::rand::SystemRandom) -> pairing::Window {
    let mut secret = [0u8; 16];
    ring::rand::SecureRandom::fill(rng, &mut secret).expect("random source");
    pairing::Window::new(secret, pairing::generate_code(rng), protocol::now())
}

/// Prints the QR code and the 12 digits, and derives the typed code's key in the background.
fn show_pairing(window: &Arc<Mutex<pairing::Window>>, fp: &[u8; 32], name: &str, port: u16) {
    let (secret, code) = {
        let w = window.lock().unwrap();
        (w.secret, w.code.clone())
    };
    let addresses: Vec<String> = local_address().into_iter().collect();
    let link = pairing::pair_link(fp, &secret, name, port, &addresses);
    println!();
    println!("To pair an iPad or iPhone: open Sill on it and tap \"{name}\", then scan this code,");
    print_qr(&link);
    println!("or tap Enter Code Instead and type:  {}", pairing::grouped(&code));
    println!("(This code works once, for 5 minutes; a new one appears here after that.)");
    // For the end-to-end checks (README, "Checks"): the code and link where a test client reads them.
    if let Some(path) = std::env::var_os("SILL_TEST_CODE_FILE") {
        let _ = std::fs::write(path, &code);
    }
    if let Some(path) = std::env::var_os("SILL_TEST_LINK_FILE") {
        let _ = std::fs::write(path, &link);
    }
    let (w, fp, c) = (window.clone(), *fp, code);
    std::thread::spawn(move || {
        let key = pairing::code_key(&c, &fp);
        let mut w = w.lock().unwrap();
        if w.code == c {
            w.code_key = Some(key);
        }
    });
}

/// Black modules on white, two rows per line of text, with the quiet zone QR readers need.
fn print_qr(text: &str) {
    let Ok(code) = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M) else {
        println!("(The link is too long for a QR code.)");
        return;
    };
    let w = code.width() as i32;
    let dark = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < w && code[(x as usize, y as usize)] == qrcode::Color::Dark;
    let q = 2;
    let mut y = -q;
    while y < w + q {
        let mut line = String::from("\x1b[30;107m");
        for x in -q..w + q {
            line.push(match (dark(x, y), dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        line.push_str("\x1b[0m");
        println!("{line}");
        y += 2;
    }
}

fn pc_name() -> String {
    let n = mdns::host_name().or_else(|| std::env::var("COMPUTERNAME").ok()).or_else(|| std::env::var("HOSTNAME").ok()).unwrap_or_default();
    let n = protocol::safe_label(&n, 64);
    if n.is_empty() { "Windows PC".into() } else { n }
}

/// This PC's address on its main network, for the QR code's link (what a device dials when it
/// can't find the PC by name).
fn local_address() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?; // nothing is sent: this only picks the outgoing interface
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
}

fn primary_rect() -> input::Rect {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        return input::Rect { left: 0, top: 0, width: GetSystemMetrics(SM_CXSCREEN), height: GetSystemMetrics(SM_CYSCREEN) };
    }
    #[allow(unreachable_code)]
    input::Rect { left: 0, top: 0, width: 1920, height: 1080 }
}

#[cfg(windows)]
fn console_setup() {
    use windows::Win32::System::Console::*;
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    unsafe {
        // Real pixels for capture and the pointer, whatever the display scaling.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Colors in the console, for the QR code.
        if let Ok(h) = GetStdHandle(STD_OUTPUT_HANDLE) {
            let mut mode = CONSOLE_MODE::default();
            if GetConsoleMode(h, &mut mode).is_ok() {
                let _ = SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
        let _ = SetConsoleTitleW(windows::core::w!("Sill for Windows"));
    }
}
