#!/usr/bin/env bash
# Exercise OXRAD against an isolated Xvfb display and save framebuffer evidence.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tools="${OXRAD_GUI_TOOLS:-$root/.tools/root}"
output="${OXRAD_GUI_OUTPUT:-$root/target/gui-smoke}"
binary="${OXRAD_GUI_BINARY:-$root/target/release/oxrad}"
mkdir -p "$output/framebuffer"
home="$(mktemp -d "$output/home.XXXXXXXX")"
export LD_LIBRARY_PATH="$tools/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export WGPU_BACKEND=vulkan HOME="$home"
export VK_DRIVER_FILES="${OXRAD_VULKAN_ICD:-$root/.tools/lavapipe.json}"

xvfb_pid=""
app_pid=""
cleanup() {
    if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then kill "$app_pid"; fi
    if [[ -n "$xvfb_pid" ]] && kill -0 "$xvfb_pid" 2>/dev/null; then kill "$xvfb_pid"; fi
}
trap cleanup EXIT
: > "$output/display"

"$tools/usr/bin/proot" \
    -b "$tools/usr/bin/xkbcomp:/usr/bin/xkbcomp" \
    -b "$tools/usr/share/X11/xkb:/usr/share/X11/xkb" \
    "$tools/usr/bin/Xvfb" -displayfd 3 -screen 0 1280x800x24 -nolisten tcp -ac \
    -fbdir "$output/framebuffer" 3> "$output/display" > "$output/xvfb.log" 2>&1 < /dev/null &
xvfb_pid=$!
for attempt in {1..30}; do
    kill -0 "$xvfb_pid" 2>/dev/null || { cat "$output/xvfb.log"; exit 1; }
    if [[ -s "$output/display" ]]; then break; fi
    sleep 1
done
if [[ ! -s "$output/display" ]]; then cat "$output/xvfb.log"; exit 1; fi
export DISPLAY=":$(cat "$output/display")"
"$tools/usr/bin/xdotool" getdisplaygeometry > /dev/null
"$binary" --site KTWX > "$output/app.log" 2>&1 < /dev/null &
app_pid=$!
window=""
for attempt in {1..30}; do
    kill -0 "$app_pid" 2>/dev/null || { cat "$output/app.log"; exit 1; }
    window="$("$tools/usr/bin/xdotool" search --pid "$app_pid" --name oxrad | head -1 || true)"
    if [[ -n "$window" ]]; then break; fi
    sleep 1
done
if [[ -z "$window" ]]; then cat "$output/app.log"; exit 1; fi
"$tools/usr/bin/xdotool" windowfocus "$window"
sleep 15

capture() {
    kill -0 "$app_pid" 2>/dev/null || { cat "$output/app.log"; exit 1; }
    "$tools/usr/bin/xwdtopnm" "$output/framebuffer/Xvfb_screen0" | \
        "$tools/usr/bin/pnmtopng" > "$output/$1.png"
}
click() {
    "$tools/usr/bin/xdotool" mousemove --window "$window" "$1" "$2" click 1
}

capture radar
click 150 354
sleep 1
capture product-menu
click 75 405
sleep 12
capture velocity
click 235 28
sleep 8
capture map
click 80 120
sleep 1
"$tools/usr/bin/xdotool" type --window "$window" --clearmodifiers --delay 400 Chicago
sleep 12
capture search
click 125 170
sleep 15
capture forecast
click 1250 28
sleep 2
capture light-theme
"$tools/usr/bin/xdotool" mousemove --window "$window" 550 400 mousedown 1 mousemove --sync 640 440 mouseup 1
"$tools/usr/bin/xdotool" click 4
sleep 2
capture pan-zoom
"$tools/usr/bin/xdotool" windowsize "$window" 1040 640
sleep 2
capture small-window
printf 'Captured GUI smoke workflow in %s\n' "$output"
