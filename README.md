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
4. A black window opens. It shows the PC's name, a big **QR code** (a square of black and white
   blocks), and a **12-digit code** under it. That's how you pair the iPad. Leave this window open
   the whole time you use Sill. Closing it stops Sill.

If the window says something went wrong, read the last lines: they say what happened in plain
words.

## Pair the iPad (once)

1. Open **Sill** on the iPad. Your PC appears in the list by its name, with **Not paired** under it.
2. Tap the PC's name. The iPad opens its camera.
3. Point the camera at the QR code in the black window on the PC. In a moment the iPad says it's
   paired and shows the PC's screen.
   - If the camera won't read the code, tap **Enter Code Instead** on the iPad and type the 12 digits
     shown under the QR code.
   - A code works once, for 5 minutes. After that the window shows a new one by itself.
4. From now on, tap the PC's name and it connects straight away. No code needed.

## Using it

Use Sill just as you would with a Mac. Its taps, right-clicks, scrolling and typing all reach
Windows, and a keyboard attached to the iPad works too.

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

- **The PC isn't in the iPad's list.** Check both are on the same Wi‑Fi. Then check the firewall:
  open the **Start** menu, type **Allow an app through Windows Firewall**, open it, click
  **Change settings**, find **SillHost** in the list, and tick both boxes next to it. Click **OK**
  and restart SillHost.
- **The PC is listed but tapping it does nothing, or it says it couldn't connect.** Same firewall
  fix as above. Also make sure the black window is still open on the PC.
- **The iPad connects but the picture stays black.** Look at the black window on the PC. A line
  starting with **Picture:** says why. "no hardware HEVC (H.265) encoder" means this PC's
  graphics chip can't make the video Sill needs.
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

Be clear on this: **nobody has run this on a real Windows PC with a real iPad yet.** That's the
next step, and it's yours, because it needs your PC and your iPad.

What has been checked:

- **The wire format against Sill's own code.** The `conformance` folder builds Sill's Swift protocol
  code and dumps the exact bytes it makes. The tests check that every message this program sends
  matches them, and that every message an iPad sends decodes here. Then Sill's own decoders read
  what this program writes, and must accept all of it. The pairing math (the code, the proofs, the
  PC ID, the QR link, the Bonjour tag) matches Sill's code to the byte.
- **Pairing and a session over Apple's own network code.** A test program built on Sill's code and
  Apple's Network framework (the same TLS the iPad uses) pairs with the typed code, connects, gets
  the window list, settings, and the video's opening frames, and gets pongs back. A device that
  isn't paired is refused with exactly the error the Mac app gives (-9825).
- **Pairing by QR code with Sill's own Python test client** (`Scripts/sillclient.py` in Sill).
- **The real program on Windows.** GitHub's Windows machine runs every test, including starting
  SillHost.exe, pairing with it, holding a session, and restarting it to check it keeps its
  identity and its paired devices.

What has **not** been checked, because GitHub's Windows machines have no graphics chip and no
screen:

- Screen capture, the HEVC encoder, and the video actually appearing on the iPad.
- Clicks and typing actually landing on Windows.
- The iPad finding the PC over Bonjour on a real Wi‑Fi network.

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
- `SILL_HOME` moves the whole storage folder (identity and paired devices), so a test never
  touches a real one. Normally it's `%LOCALAPPDATA%\Sill`, with the private key sealed by Windows
  (DPAPI) so only your Windows account can read it.

The source files: `src/protocol.rs` (the wire format), `src/pairing.rs` (codes, proofs, the
pairing window), `src/identity.rs` (the key, the certificate, paired devices), `src/tls.rs` and
`src/server.rs` (the network door and sessions), `src/video/` (capture and HEVC), `src/input.rs`
(clicks and keys, with the key table), and `src/mdns.rs` (Bonjour).

## Credits and license

Sill is by its authors at [github.com/Saffsanity/sill](https://github.com/Saffsanity/sill), under the
Apache License 2.0. This program is a separate Windows host written to work with Sill's iOS app; it
contains no code from Sill's apps, but its protocol follows Sill's design and its tests build
Sill's code as the reference. Licensed under the Apache License 2.0 (see `LICENSE`).
