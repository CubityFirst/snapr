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

Plus 8 colours, 3 sizes and undo/redo (`Ctrl+Z` / `Ctrl+Y`). **Enter** / ✓
saves the whole monitor under the cursor, **Ctrl+C** copies it to the clipboard
without saving a file. Turn off *Show the annotation toolbar* in settings for a
plain region capture.

## Main window

Open it from the tray (left-click on Windows, or *Recent screenshots…*). The
bar on the left has *Capture*, *Recent*, *Tools*, *Settings* and, at the
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
    its hex code. Arrow keys move the pointer one pixel (Shift: ten). The
    colour is shown as HEX, `rgb()` and `hsl()`, each with *Copy*, and recent
    picks are kept as swatches while the window is open.
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
file. The object key uses the same placeholders as file names
(default `%y-%mo/%rna{10}.png`); **Public URL** is the base for links, e.g. an
`https://pub-....r2.dev` address or your own domain. After an upload the link
is copied to the clipboard (optional) and the Recent page gets a *Copy link*
button. Right-click an uploaded screenshot there and choose *Delete remotely* to
remove the uploaded copy (the local file is kept); *Clear recent* empties the
list without deleting any files.

Secret access keys are kept in the system credential store (Windows Credential
Manager, macOS Keychain, Secret Service on Linux), never in `config.toml`.
Requests are signed with AWS Signature V4, checked against AWS's published
examples. To try a real bucket from the command line, set
`SNAPR_TEST_S3_ENDPOINT`, `_REGION`, `_BUCKET`, `_ACCESS_KEY_ID`,
`_SECRET_ACCESS_KEY` (and `_PATH_STYLE=1` for R2/MinIO) and run
`cargo test live_bucket -- --ignored`.

Settings are stored in `<config dir>/snapr/config.toml`
(`%APPDATA%\snapr` on Windows, `~/Library/Application Support/snapr` on macOS,
`~/.config/snapr` on Linux). Set `SNAPR_CONFIG_DIR` to use another folder, e.g.
for a portable copy or a second, independent instance.

| Setting            | Default                  |
|--------------------|--------------------------|
| Capture hotkey     | `Alt + Shift + KeyS` (or press *Record* and type a combination) |
| Save folder        | `<Pictures>/snapr`       |
| Sub-folder         | none                     |
| File name          | `%rna{10}` (+ `.png`)    |
| Copy to clipboard  | on                       |
| Annotation toolbar | on                       |
| Play sounds        | on (sounds are synthesized in code, no audio assets) |
| Corner preview     | on: click opens the link (or image), middle-click copies, right-click closes |
| Overlay frame rate | match monitor (vsync), or a custom limit |

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
  or bind `snapr --once` to a compositor shortcut.

## License

The source code is licensed under the
[PolyForm Noncommercial License 1.0.0](LICENSE): free to use, modify and
share for any noncommercial purpose. Commercial use needs separate permission.

The snapr name and logo are not covered by that license and remain all rights
reserved; see [TRADEMARKS.md](TRADEMARKS.md).
