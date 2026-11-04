# Sill for Windows

Sill for Windows lets the **Sill** app on an iPad or iPhone show your Windows PC's screen and
control it: tap to click, drag to move the pointer, and type on the iPad's keyboard. It speaks the
same language as Sill's own Mac app, so the normal Sill app from the App Store works with it as it
is. Nothing needs changing on the iPad.

This is version 0.1, the first working version. Read **What's not done yet** before you rely on it.

## What you need

- A Windows 10 or Windows 11 PC. Its graphics chip has to be able to make HEVC video (also called
  H.265). Almost every PC from 2016 or later can: Intel 6th generation or newer, NVIDIA GTX 950 or
  newer, and AMD RX 400 or newer.
- An iPad or iPhone with the **Sill** app from the App Store.
- Both on the **same Wi‑Fi network**.

## Install it

There is no installer yet. The program is one file, `SillHost.exe`, which GitHub builds for you
every time the code changes. You download it from GitHub.

You do this part by hand because downloading needs your GitHub sign-in, which only you have.

1. On the Windows PC, open a web browser and go to
   **https://github.com/TanookiStudios/sill-windows/actions**.
   If GitHub asks you to sign in, sign in as TanookiStudios.
2. You'll see a list of builds. Click the top one named **Build** that has a **green check mark**
   next to it.
3. Scroll down to the section called **Artifacts**. Click **SillHost-windows**.
   A file called `SillHost-windows.zip` downloads.
4. Open your **Downloads** folder. Right-click `SillHost-windows.zip` and choose **Extract All…**,
   then click **Extract**. A folder opens with `SillHost.exe` in it.
5. Move that folder somewhere you'll find it again, such as your Desktop.

## Start it the first time

1. Double-click **SillHost.exe**.
2. Windows may show a blue box that says **"Windows protected your PC"**. That's because the program
   isn't signed by a company yet. Click **More info**, then **Run anyway**.
3. Windows may ask whether to let **SillHost** use the network ("Windows Defender Firewall has
   blocked some features"). Tick **Private networks** and click **Allow access**. If you skip this,
   the iPad won't be able to find the PC.
4. Two windows open. The black one is Sill itself: leave it open the whole time you use Sill, because
   closing it stops Sill. The white one, **Pair with Sill**, shows a big **QR code** (a square of
   black and white blocks) and a **12-digit code**. That's how you pair the iPad.
5. Within about 10 seconds the black window should say **Picture check passed**. That means this PC
   can capture its screen and make the video.

If something goes wrong, read the last lines of the black window: they say what happened in plain
words. Everything it says is also saved in **Sill-log.txt**, next to SillHost.exe. Your pairing code
is never saved there.

## Pair the iPad (once)

1. Open **Sill** on the iPad. Your PC appears in the list by its name, with **Not paired** under it.
2. Tap the PC's name. The iPad opens its camera.
3. Point the camera at the QR code in the white window on the PC. In a moment the iPad says it's
   paired and shows the PC's screen, and the white window goes away.
   - If the camera won't read the code, tap **Enter Code Instead** on the iPad and type the 12 digits
     shown under the QR code.
   - A code works once, for 5 minutes. After that the window shows a new one by itself.
   - If you closed the white window, tapping the PC's name on the iPad brings it back.
4. From now on, tap the PC's name and it connects straight away. No code needed.

## Using it

Use Sill just as you would with a Mac. Its trackpad, taps, right-clicks, scrolling and typing all
reach Windows, and a keyboard attached to the iPad works too.

Mac keys become Windows keys like this:

| On the iPad keyboard      | On Windows                         |
|---------------------------|------------------------------------|
| Command (⌘)               | Ctrl                               |
| Control (⌃)               | Ctrl                               |
| Option (⌥)                | Alt                                |
| Shift (⇧)                 | Shift                              |
| Command-Space             | the Windows key (Start and search) |
| Delete (⌫)                | Backspace                          |
| Forward Delete (⌦)        | Delete                             |
| Return, Tab, Esc, arrows, F1–F12, Home, End, Page Up, Page Down | the same keys |

So ⌘C copies, ⌘V pastes, ⌘Z undoes, ⌘S saves, and ⌥Tab switches windows, just as Ctrl+C,
Ctrl+V, Ctrl+Z, Ctrl+S and Alt+Tab do on Windows.

To stop, click the black window on the PC and press **Ctrl+C**, or close it. The iPad then says
the PC quit.

## If something goes wrong

- **The PC isn't in the iPad's list.** Check both are on the same Wi‑Fi. If the black window says
  Windows calls this network **Public**, open **Settings**, then **Network & internet**, then **Wi‑Fi**
  (or **Ethernet**), then your network's properties, and choose **Private network**. Then check the firewall:
  open the **Start** menu, type **Allow an app through Windows Firewall**, open it, click
  **Change settings**, find **SillHost** in the list, and tick both boxes next to it. Click **OK**
  and restart SillHost.
- **The PC is listed but tapping it does nothing, or it says it couldn't connect.** Same firewall
  fix as above. Also make sure the black window is still open on the PC.
- **The black window says Picture check FAILED, or the iPad connects but stays black.** The line
  says why. "no hardware HEVC (H.265) encoder" means this PC's graphics chip can't make the video
  Sill needs.
- **The picture freezes when a "Do you want to allow this app to make changes" box appears, or on
  the lock screen.** Windows hides those screens from every screen-sharing program. The picture
  comes back once they're gone.
- **Clicks don't work in one particular window.** That window belongs to a program running as
  administrator. Windows doesn't let normal programs click into those. Right-click SillHost.exe
  and choose **Run as administrator** if you need to control them.
- **To unpair every iPad and iPhone,** open the folder with SillHost.exe, click the address bar,
  type `cmd` and press Enter. In the window that opens, type `SillHost --forget-devices` and press
  Enter. Each device pairs again with a new code next time.

## What's not done yet

Version 0.1 does the core job: see the whole main screen and control it. These Mac-app features
are not in it yet:

- Streaming a single window instead of the whole screen, window thumbnails, and the app list.
- The PC's menus on the iPad, the clipboard (copy on one, paste on the other), and sound.
- Trackpad gestures (three- and four-finger swipes).
- More than one monitor: only the main one is shown.
- The PC's own mouse pointer in the picture. The iPad draws its own pointer instead.
- Changing quality or frame rate from the iPad's Settings panel. The panel shows the PC's
  settings, but changes from the iPad are not applied yet. Start SillHost with `--bitrate` and
  `--max-size` to change them.
- Holding a key down to repeat it. The iPad types the key once.
- Pairing over the USB cable, and reaching the PC from outside your home (Remote Access).
- An installer, a signed program, and starting automatically when Windows starts.

## What has been tested, and how

**Nobody has run this on a real PC with a real iPad yet.** One piece is still unproven anywhere: the
step on the PC's graphics chip that turns the screen into HEVC video. It needs a real graphics chip,
so its first real run will be on your PC. `MORNING.md` is the test sheet for that.

Everything else has run for real:

- **The real Sill app (version 0.6.1 from Sill's source, on a simulated iPad) against this
  program.** It found the program over Bonjour, showed it as Not paired, asked to pair, took the code,
  connected, and showed real HEVC video at 1920×1080 and 2560×1440, including frames split into
  slices and video from Apple's hardware encoder. Its taps, trackpad, scrolling, typing and keys
  all arrived correctly. It reconnected from its list with no code, and when the program quit it said
  so properly. Video came from a test file on a Mac, not from Windows.
- **Real input on real Windows.** GitHub's Windows machine has a real desktop. A test sends a device's
  input through the running SillHost.exe and reads back what a real window received: a left click on the exact
  spot, a right click, the wheel, typing with an accent and an emoji, Command-A, Backspace,
  an arrow key and Shift.
- **Screen capture on real Windows**, as far as one image into memory.
- **The pairing window on real Windows**, photographed, with the QR code in the photo read by a
  standard QR reader.
- **Bonjour on real Windows.** A separate Bonjour client saw the name, port, addresses and both
  TXT keys.
- **Pairing, sessions, and keeping the identity across a restart on real Windows.**
- **The wire format against Sill's own code.** The `conformance` folder builds Sill's Swift protocol code.
  Every message this program sends matches its bytes, every message an iPad sends decodes here, and
  Sill's own decoders accept what this program writes.

## For developers

The host is Rust. Everything platform-specific is behind `cfg(windows)`; on other systems it builds
with a stand-in video source, which is what the tests use.

- `cargo test`: unit tests, the conformance tests against `tests/fixtures/swift`, and the
  end-to-end test that runs the real program.
- `cd conformance && swift run SillConformance dump ../tests/fixtures/swift`: fresh fixtures from
  Sill's Swift code (macOS; `SILL_PATH` points at a checkout of
  [Saffsanity/sill](https://github.com/Saffsanity/sill), default `../../sill-reference`).
- `swift run SillConformance check ../tests/fixtures/rust`: Sill's decoders over what Rust wrote.
- `swift run SillConformance dial PORT CODEFILE`: a device's side over Network.framework against a
  running host started with `SILL_TEST_CODE_FILE=CODEFILE`.
- `SillHost --check` tests screen capture and the encoder, then exits.
- `SILL_TEST_VIDEO=clip.hevc` (not on Windows) plays an Annex B file with access unit delimiters
  through the same conversion and fan-out as the encoder's output, for testing with the real app.
- `SILL_HOME` moves the whole storage folder (identity and paired devices), so a test never
  touches a real one. Normally it's `%LOCALAPPDATA%\Sill`, with the private key sealed by Windows
  (DPAPI) so only your Windows account can read it.

The source files: `src/protocol.rs` (the wire format), `src/pairing.rs` (codes, proofs, the
pairing window), `src/identity.rs` (the key, the certificate, paired devices), `src/tls.rs` and
`src/server.rs` (the network door and sessions), `src/video/` (capture and HEVC), `src/input.rs`
(clicks and keys, with the key table), `src/mdns.rs` (Bonjour), `src/pair_window.rs` (the QR
window), and `src/log.rs` (the log file).

## Credits and license

Sill is by its authors at [github.com/Saffsanity/sill](https://github.com/Saffsanity/sill), under the
Apache License 2.0. This program is a separate Windows host written to work with Sill's iOS app; it
contains no code from Sill's apps, but its protocol follows Sill's design and its tests build
Sill's code as the reference. Licensed under the Apache License 2.0 (see `LICENSE`).
