#!/usr/bin/env sh
# Renders assets/readme-header.html to assets/readme-header.png (the README
# banner) at 2x with headless Chrome or Edge.
set -e
root="$(cd "$(dirname "$0")/.." && pwd)"
browser="${BROWSER:-}"
for b in chrome google-chrome chromium msedge \
  "/c/Program Files/Google/Chrome/Application/chrome.exe" \
  "/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe" \
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"; do
  [ -n "$browser" ] && break
  command -v "$b" >/dev/null 2>&1 && browser="$b"
done
[ -n "$browser" ] || { echo "no Chrome/Edge found; set BROWSER" >&2; exit 1; }
html="$root/assets/readme-header.html"
png="$root/assets/readme-header.png"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) html="$(cygpath -m "$html")"; png="$(cygpath -w "$png")";; esac
"$browser" --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=2 \
  --default-background-color=00000000 --window-size=1280,320 \
  --screenshot="$png" "file:///${html#/}"
