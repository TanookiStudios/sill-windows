# Testing Sill for Windows

This takes about 10 minutes. It's you doing it because it needs your PC and your iPad, and neither
is something I can reach from here.

Do one step at a time. If a step goes wrong, stop there and go to **If it goes wrong** at the bottom.

## 1. Get the program onto the PC

1. On the PC, open a web browser and go to **https://sillforwindows.tanookistudios.com**.
2. Scroll to the bottom and click **Single program**. `SillHost.exe` downloads.
3. Open your **Downloads** folder. `SillHost.exe` is there.

## 2. Start it

1. Double-click **SillHost.exe**.
2. If a blue box says **Windows protected your PC**, click **More info**, then **Run anyway**.
3. If Windows asks about the firewall, tick **Private networks** and click **Allow access**.

**You should see** a black window with text in it, and a white window called **Pair with Sill** with a
big square QR code and 12 digits.

Within about 10 seconds, the black window should say **Picture check passed**.
This is the first big moment. That line means this PC can capture its screen and make the video.

## 3. Pair the iPad

1. Make sure the iPad is on the same Wi‑Fi as the PC.
2. Open **Sill** on the iPad. Your PC's name should be in the list, with **Not paired** under it.
3. Tap it. The iPad offers to scan a code.
4. Point the iPad at the white window's QR code. If the camera won't read it, tap
   **Enter Code Instead** and type the 12 digits.

**You should see** your Windows screen appear on the iPad, and the white window disappear.

## 4. Try it

- Drag a finger on the iPad's trackpad area, and watch the pointer move on the PC.
- Tap to click something.
- Scroll with two fingers.
- Open Notepad on the PC, tap into it, tap the keyboard button on the iPad, and type something.

After about 10 seconds, the black window should have a line like
**iPad shows 58 frames a second, 20 ms behind this PC**. That's the iPad's own report of what it's
showing, so tell me those numbers.

## 5. Try the extras

Do these in any order. Tell me which ones work.

- **One window:** open a program on the PC, tap **Apps** on the iPad, and tap that program's window.
  The iPad should show just that window. Tap **Desktop** to go back.
- **Open an app:** in **Apps**, tap something under **All apps**.
- **Clipboard:** copy some text on the PC, then paste it on the iPad, and try the other way around.
  **Share Clipboard** has to be on in the iPad's Settings.

- **Gestures:** swipe up with three fingers on the trackpad area. Windows' Task View should open.
- **Quality:** in the iPad's Settings, change the quality. The picture should blink and come back.

- **Menus:** on the PC, right-click the Start button and choose **Device Manager**. On the iPad, tap
  **Apps**, pick Device Manager's window, then tap **Menus** in Sill's bar. You should see File, Action,
  View and Help. Tap **View**, then **Devices by connection**. The list on the PC should change.
- **USB cable (optional):** install **Apple Devices** from the Microsoft Store on the PC, plug the iPad
  into the PC, and tap **Trust** if the iPad asks. Then look at Sill's list of computers on the iPad.
  If your PC says **Wired** under its name, the cable works. If it doesn't, that's fine, just tell me.

## 6. Tell me how it went

Tell me which steps worked. If they all did, tell me whether it felt smooth or laggy.

## If it goes wrong

Tell me the step number and what you saw. The most useful thing you can give me is the file called
**Sill-log.txt**, which sits next to `SillHost.exe`. Double-click it, press **Ctrl+A**, then **Ctrl+C**,
and paste it to me. A phone photo of the black window works too.

It holds what the program saw and tried, and never contains your pairing code.

Single windows are also unproven on a real PC, so don't worry if those need a fix.

These are the two likeliest problems:

- **The PC isn't in the iPad's list.** Look for a line in the black window saying Windows calls this
  network **Public**. If it's there, open **Settings**, then **Network & internet**, then **Wi‑Fi**
  (or **Ethernet**), then your network's properties, and choose **Private network**. Then quit
  SillHost by closing the black window, and start it again.
- **It says Picture check FAILED.** Don't try to fix it. Just send me the log, because the rest of that
  line says exactly what failed.
