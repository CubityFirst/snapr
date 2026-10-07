# snapr

Cross-platform region screenshot tool. It lives in the system tray: press the
hotkey, every monitor freezes, drag a region, and the result is saved as a PNG
and copied to the clipboard.

```
cargo build --release      # -> target/release/snapr(.exe)
```

While frozen, **drag** a region and let go to take the screenshot (works across
monitors). **Shift** keeps the region square, holding **Ctrl** moves the region
while you drag it, and a plain **click** takes the whole monitor.
**Right-click** / **Esc** cancels. A shutter sound plays on capture.

Pressing the hotkey again during a capture freezes the screen again, overlay
and toolbar included, so the capture UI itself can be captured.

## Annotating

As soon as the screen freezes, a toolbar appears at the top of the monitor
you're on. Every capture starts with the Region tool; pick another tool to
draw on the frozen screen first, then switch back to Region (`S`) and drag the
region; the screenshot includes your annotations.

| Tool | Key | |
|------|-----|-|
| Region | `S` | the default: drag to capture |
| Pen | `P` | freehand |
| Line | `L` | Shift snaps to 45° |
| Arrow | `A` | Shift snaps to 45° |
| Box | `R` | Shift makes a square |
| Ellipse | `E` | Shift makes a circle |
| Highlighter | `H` | translucent marker |
| Blur | `B` | blurs the dragged area |
| Pixelate | `X` | pixelates the dragged area |
| Image redaction | `I` | covers the dragged area with an image chosen in Settings (stretched to fit, or cropped to keep its proportions); black boxes if none is set |

Plus 8 colours, 3 sizes and undo/redo (`Ctrl+Z` / `Ctrl+Y`). **Enter** / ✓
saves the whole monitor under the cursor, **Ctrl+C** copies it to the clipboard
without saving a file. Turn off *Show the annotation toolbar* in settings for a
plain region capture.

## Main window

Open it from the tray (left-click on Windows, or *Recent screenshots…*). The
bar on the left has *Capture*, *Recent*, *Tools*, *Stats*, *Settings* and, at the
bottom, *Open screenshots folder*.

- **Recent** shows a large preview of the selected screenshot (the newest by
  default) with *Open*, *Copy*, *Show in folder* and *Delete* (to the Recycle
  Bin), and a grid of earlier ones: click to preview, double-click to open,
  right-click for the same actions. Saved screenshots are remembered in
  `history.txt` next to the settings.
- **Combine images** on the Recent page: drag one screenshot onto another and
  drop it on *Horizontal* or *Vertical*, or Ctrl-click several (they're
  numbered and combined in the order you click them; Esc clears the selection)
  and right-click one for *Combine horizontally / vertically*. The
  result is a new screenshot, saved, copied and uploaded like a capture.
  Smaller images are centred, and the space around them is filled with the
  most common colour along their own edge.
- **Tools** holds capture-related utilities, one tab each:
  - **QR code**: *Scan region…* freezes the screen; drag a region around a
    code (or click to search a whole window) and the text of every code found
    is shown with *Copy* and, for links, *Open link*. Tiny and light-on-dark
    codes work too. Below that, type text to make a QR code, then *Copy
    image* or *Save as…* a PNG.
  - **Colour picker**: *Pick colour…* freezes the screen (undimmed) with a
    magnifier beside the cursor; click a pixel (or press **Enter**) to copy
    its hex code. Arrow keys move the pointer one pixel (Shift: ten); scrolling
    zooms the magnifier, or resizes it (see *Scrolling the magnifier*). The
    colour is shown as HEX, `rgb()` and `hsl()`, each with *Copy*, and recent
    picks are kept as swatches while the window is open.
  - **Pin to screen**: *Pin region…* freezes the screen; drag a region (or
    click a window) and it's pinned right where it was, in a borderless
    window that stays on top of everything. *Pin from clipboard* and
    *Pin image file…* pin other images (in the middle of the screen, shrunk
    to fit), and *Pin* on the Recent page (or *Pin to screen* in a
    screenshot's right-click menu) pins a saved screenshot. Drag a pin to
    move it; scroll, `+` / `-`, or drag a corner to make it bigger or smaller
    (it keeps its shape and stops at 100% on the way past); double-click or
    `0` returns it to its real size. Pins start on top of every other
    window; `T` or the pin button switches that off (it then shows in the
    taskbar so it can be found again) and back on. Hovering shows the zoom
    level and buttons for on top, *Copy* and *Close*; **Ctrl+C** copies
    it, **Ctrl+S** saves it as a PNG, and **right-click** / **Esc** closes
    it. *Close all* on the tab closes every
    pin.

  Any tool can have global hotkeys that run it straight away, from any app:
  add them under *Settings → Hotkeys → Tool hotkeys* (pick the tool, press
  *Record*, type the combination), or with *Add a hotkey…* on the
  tool's tab. A tool can have more than one, and *Remove* deletes one.
- **Stats** counts captures: screenshots vs recordings, charted by time of
  day and day of the week, plus *Tool Use* (QR scans, codes read, colours
  picked, images pinned). It's kept in `stats.toml` next to the settings.
- **Settings** opens by itself on first run, or whenever something needs
  attention (e.g. the hotkey couldn't be registered).

Launching snapr again while it's running opens the running instance's window
(Windows).

## Destinations

The *Destinations* tab of Settings sets where each screenshot goes: **Save to folder**,
**Copy image to clipboard**, and any number of **S3-compatible uploads**
(Cloudflare R2, Amazon S3, MinIO, Backblaze B2, ...). Add one with
*+ Cloudflare R2* (replace `ACCOUNT_ID` in the endpoint) or *+ S3-compatible*,
fill in the bucket and keys, and press **Test** to upload and delete a tiny
file. **Test upload** sends snapr's icon to every ticked destination, named
as a screenshot would be, and shows its link; that file stays. The object
key uses the same placeholders as file names (default `%y-%mo/%rna{10}.png`); **Public URL** is the base for links, e.g. an
`https://pub-....r2.dev` address or your own domain. After an upload the link
is copied to the clipboard (optional) and the Recent page gets a *Copy link*
button. Right-click an uploaded screenshot there and choose *Delete remotely* to
remove the uploaded copy (the local file is kept); *Clear recent* empties the
list without deleting any files.

*+ Add Pomf upload* adds a [Pomf](https://github.com/pomf/pomf)-compatible
file host instead (version 1 of its API: pomf.lain.la, uguu.se, qu.ax, ...).
Give it the host's upload address, e.g. `https://pomf.lain.la/upload.php`;
there are no keys. The host picks the file's name and link (set **Public
URL** to put links under a domain of your own). Pomf can't delete uploads,
so **Test** uploads snapr's icon and leaves it there, and those screenshots
don't offer *Delete remotely*.

Secret access keys are kept in the system credential store (Windows Credential
Manager, macOS Keychain, Secret Service on Linux), never in `config.toml`.
Requests are signed with AWS Signature V4, checked against AWS's published
examples. To try a real bucket from the command line, set
`SNAPR_TEST_S3_ENDPOINT`, `_REGION`, `_BUCKET`, `_ACCESS_KEY_ID`,
`_SECRET_ACCESS_KEY` (and `_PATH_STYLE=1` for R2/MinIO) and run
`cargo test live_bucket -- --ignored`.

Changes on the Settings pages save as you make them (once a change is valid).
Settings are stored in `<config dir>/snapr/config.toml`
(`%APPDATA%\snapr` on Windows, `~/Library/Application Support/snapr` on macOS,
`~/.config/snapr` on Linux). Set `SNAPR_CONFIG_DIR` to use another folder, e.g.
for a portable copy or a second, independent instance.

| Setting            | Default                  |
|--------------------|--------------------------|
| Capture hotkey     | `Alt + Shift + KeyS` (or press *Record* and type a combination) |
| Save folder        | `<Pictures>/snapr`       |
| Sub-folder         | `%y-%mo` (e.g. `2026-10`) |
| File name          | `%rna{10}` (+ `.png`)    |
| Copy to clipboard  | on                       |
| Annotation toolbar | on                       |
| Play sounds        | on (sounds are synthesized in code, no audio assets) |
| Sound volume       | 100% (dragging the slider plays the chime when you let go) |
| Corner preview     | on: click opens the link (or image), middle-click copies, right-click closes |
| Overlay frame rate | match monitor (vsync), or a custom limit |
| Beside the crosshair | off; any of *Position* (screen X/Y), *Colour* (hex and RGB) and *Magnifier*, shown next to the cursor while selecting a region |
| Scrolling the magnifier | *Zooms it* (fewer, bigger pixels, or more, smaller ones); *Resizes it* makes it bigger, or smaller until it's hidden |
| Check for updates  | on (see [Updates](#updates)) |

### Naming templates

Templates use ShareX's syntax. Random tokens repeat with `{n}`, e.g. `%rna{10}`.

| Token | Value |
|-------|-------|
| `%rna` | random base56 character (alphanumerics without 0/O/o, 1/I/l) |
| `%ra` / `%rn` / `%rx` | random alphanumeric / digit / hex character |
| `%guid` | GUID |
| `%i`, `%i{4}` | counter that goes up with each screenshot, optionally zero-padded |
| `%y` `%yy` `%mo` `%mon` `%mon2` `%d` | year, 2-digit year, month, month name, short month name, day |
| `%h` `%mi` `%s` `%ms` `%pm` | hour (12-hour when `%pm` is present), minute, second, millisecond, AM/PM |
| `%w` `%w2` `%wy` `%unix` | weekday, short weekday, week of year, Unix timestamp |
| `%pn` `%t` | process and title of the window focused when the hotkey was pressed |
| `%width` `%height` | image size |
| `%un` `%cn` | user and computer name |

Unknown `%` tokens are kept as text. Old `{random}`-style templates in
`config.toml` are converted when it loads.

`/` in a template creates nested folders, e.g. sub-folder `%pn/%y/%mo`
gives `chrome/2026/10/k3M9x2P7qa.png`. Substituted values are sanitized and
can't escape the base folder, and existing files are never overwritten.

## Screen recordings

Recordings are saved next to your screenshots as **MP4** (H.264 + AAC; plays
everywhere) or, on Windows and Linux, **WebM** (VP9 + Opus; smaller, plays in
browsers), picked under *Settings → Recording*. Windows and macOS
use the system's own encoders; WebM on Windows needs the VP9 Video
Extensions (installed with Windows 10/11).

A region can span several monitors, like a screenshot can; anything in it
that isn't on a monitor (a gap in an uneven layout) comes out black.

While recording, a dashed border surrounds the region, with a bar below it
(timer, *Stop*, *Pause*, *Restart*, *Abort*). The border is red while the
encoder starts (FFmpeg or the graphics card can take a second), green once
it's recording, and amber while paused. The timer starts when it turns green.

If [FFmpeg](https://ffmpeg.org) is installed, *Encoder: FFmpeg* (Windows and
macOS) records MP4 with x264 and WebM with libvpx instead. They're sharper
for the file size, especially on text, but use more CPU. FFmpeg isn't
bundled; snapr finds it on `PATH`, or use the path set in Settings, where
**Test** checks that it runs and has the encoder it needs. Without it,
recordings use the system's encoder and say so. AV1 and HDR recordings
are still made by the graphics card.

With FFmpeg in use, Settings also has x264's *Preset* (default `veryfast`)
and *Quality* (CRF, default 23) for MP4, libvpx's *Speed* (default 8) and
*Quality* (CRF, default 32, capped at the usual bit rate) for WebM, and
*Extra arguments* added after snapr's own, so they override them (e.g.
`-tune stillimage`).

On Windows, **MP4 (AV1)** is sharper for the same file size: AV1 + AAC,
encoded by the graphics card (NVIDIA RTX 40, AMD RX 7000, Intel Arc or
newer; without one it records H.264 and says so). It plays in browsers,
Discord and Windows' own apps (with the free AV1 Video Extension), but not
on older devices. An HDR display is still recorded as HDR10 HEVC.

**HDR displays (Windows):** screenshots of a display in HDR mode are captured
in full range and mapped to SDR, with your *SDR content brightness* as white,
so they look like the screen. Recordings of an HDR display are HDR10 MP4s
(10-bit HEVC, made by the graphics card; they start about a second late
while it gets ready). Turn off *Record in HDR* for SDR recordings instead.
A region spanning more than one display is recorded in SDR.

## Updates

snapr looks for a new release on GitHub 15 seconds after it starts and every
12 hours after that. When there is one, it downloads this platform's build,
checks it against the release's `SHA256SUMS`, and puts it in place of the
program file; the running copy carries on, and the new version runs from the
next start (or straight away with *Restart now*). *Settings → General →
Updates* has *Do not check for updates* and *Check for updates now*, which
works either way. Pre-releases aren't offered, and development (debug)
builds only check when asked, and say what's available without installing
it.

The program has to be able to replace its own file: a copy installed
somewhere only an administrator can write to (e.g. `/usr/bin`) reports
*Couldn't update* instead.

### Releasing

Bump `version` in `Cargo.toml`, commit, then tag and push:

```
git tag v0.2.0
git push origin v0.2.0
```

The *Release* workflow (`.github/workflows/release.yml`) builds Windows
x86-64, macOS (Apple silicon only) and Linux x86-64, and publishes them
as a GitHub release named after the tag, with generated notes and a
`SHA256SUMS` file. It refuses a tag that doesn't match `Cargo.toml`'s
version (such a build would keep offering itself as an update). A tag with a
hyphen, e.g. `v0.2.0-rc.1`, is published as a pre-release.

## Command line

```
snapr --once       # capture immediately with the saved settings, then exit
snapr --settings   # open the settings window on start
```

## Platform notes

- **Windows**: works out of the box.
- **macOS**: grant *Screen Recording* (capture) and *Accessibility* (global
  hotkey) permissions. The tray icon lives in the menu bar.
- **Linux**: the tray uses StatusNotifierItem (KDE and most desktops; GNOME
  needs the AppIndicator extension). Without a tray the settings window is
  shown instead. On Wayland, global hotkeys need the user in the `input` group,
  or bind `snapr --once` to a compositor shortcut. Screen recording and video
  previews need [FFmpeg](https://ffmpeg.org) (on `PATH`, or set its path in
  Settings); Windows and macOS encode and play videos with the system's own
  codecs.

## License

The source code is licensed under the
[PolyForm Noncommercial License 1.0.0](LICENSE): free to use, modify and
share for any noncommercial purpose. Commercial use needs separate permission.

The snapr name and logo are not covered by that license and remain all rights
reserved; see [TRADEMARKS.md](TRADEMARKS.md).
