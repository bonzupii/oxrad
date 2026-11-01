#!/usr/bin/env bash
# Runs the real application on a private X display, drives it with input
# events, captures the framebuffer, and asserts on what came back.
#
# Usage: scripts/gui.sh <scenario> [output-directory]
#
# Scenarios:
#   radar     the radar workspace as it opens
#   map       the geographic workspace, reached by clicking the Map tab
#   measure   the map workspace with a four-corner area measurement drawn
#   pan       the radar workspace after a 240 px drag east
#   zoom      the radar workspace after ten scroll-wheel zoom steps
#   small     the radar workspace in a 940x680 window
#
# The measure scenario clicks fixed points inside the default 1280x800 layout
# and then counts pixels in the measurement stroke colour, so a polygon that
# silently stopped drawing fails the run.
#
# Requires Xvfb, xdotool, xwdtopnm, pnmtopng, pnmtoplainpnm. It looks for them
# on PATH first and then under .tools/root/usr/bin, which is where the
# documented headless environment keeps them. A missing tool fails the run: an
# absent check is not a passing check.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
scenario="${1:-radar}"
output="${2:-$root/target/gui}"
screen="${OXRAD_GUI_SCREEN:-1280x800}"
binary="${OXRAD_GUI_BINARY:-$root/target/release/oxrad}"

# The documented headless environment keeps its tools here; some of them are
# wrappers that call their siblings, so the directory belongs on PATH.
if [[ -d "$root/.tools/root/usr/bin" ]]; then
    export PATH="$root/.tools/root/usr/bin:$PATH"
fi

fail() {
    printf 'gui: %s\n' "$1" >&2
    exit 1
}

find_tool() {
    local name="$1"
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return 0
    fi
    if [[ -x "$root/.tools/root/usr/bin/$name" ]]; then
        printf '%s\n' "$root/.tools/root/usr/bin/$name"
        return 0
    fi
    return 1
}

for tool in Xvfb xdotool xwdtopnm pnmtopng pngtopnm pnmtoplainpnm pamcut; do
    found="$(find_tool "$tool")" || fail "$tool is required and was not found"
    printf -v "tool_$tool" '%s' "$found"
done
[[ -x "$binary" ]] || fail "$binary is not executable; build it first"

mkdir -p "$output"
home="$(mktemp -d "$output/home.XXXXXXXX")"
log="$output/$scenario.log"
png="$output/$scenario.png"
xvfb_log="$output/xvfb.log"
display_file="$output/display"

# A display number per run keeps parallel runs from colliding.
display_number=$((90 + ($$ % 60)))
rm -f "$display_file"

# proot and Xvfb need the extracted runtime libraries before they start.
if [[ -d "$root/.tools/root/usr/lib/x86_64-linux-gnu" ]]; then
    export LD_LIBRARY_PATH="$root/.tools/root/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

xkb="$root/.tools/root/usr/bin/xkbcomp"
xvfb_args=(-displayfd 3 -screen 0 "${screen}x24" -nolisten tcp -ac -fbdir "$output")
if [[ -x "$xkb" ]] && [[ -x "$root/.tools/root/usr/bin/proot" ]]; then
    "$root/.tools/root/usr/bin/proot" \
        -b "$xkb:/usr/bin/xkbcomp" \
        -b "$root/.tools/root/usr/share/X11/xkb:/usr/share/X11/xkb" \
        "$tool_Xvfb" "${xvfb_args[@]}" 3> "$display_file" > "$xvfb_log" 2>&1 < /dev/null &
else
    "$tool_Xvfb" "${xvfb_args[@]}" 3> "$display_file" > "$xvfb_log" 2>&1 < /dev/null &
fi
xvfb_pid=$!
app_pid=""
cleanup() {
    [[ -n "$app_pid" ]] && kill "$app_pid" >/dev/null 2>&1 || true
    kill "$xvfb_pid" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 30); do
    [[ -s "$display_file" ]] && break
    sleep 1
done
[[ -s "$display_file" ]] || fail "Xvfb did not report a display"
export DISPLAY=":$(head -1 "$display_file")"
export XAUTHORITY=/dev/null
export HOME="$home"
export WGPU_BACKEND="${WGPU_BACKEND:-vulkan}"
if [[ -f "$root/.tools/lavapipe.json" ]]; then
    export VK_DRIVER_FILES="$root/.tools/lavapipe.json"
fi

"$binary" --site KTWX > "$log" 2>&1 < /dev/null &
app_pid=$!

window=""
for _ in $(seq 1 40); do
    window="$("$tool_xdotool" search --name '^oxrad' 2>/dev/null | head -1 || true)"
    [[ -n "$window" ]] && break
    sleep 1
done
[[ -n "$window" ]] || fail "the application never opened a window"
# The first frames carry basemap work and the radar fetch; wait for them.
sleep "${OXRAD_GUI_SETTLE:-14}"

click() {
    "$tool_xdotool" mousemove "$1" "$2" click 1
    sleep 2
}
scroll() {
    local steps="$1"
    "$tool_xdotool" mousemove 700 450
    for _ in $(seq 1 "$steps"); do
        "$tool_xdotool" click 4
        sleep 0.2
    done
    sleep 2
}
drag() {
    local dx="$1" dy="$2" steps=10
    "$tool_xdotool" mousemove 700 450
    "$tool_xdotool" mousedown 1
    for _ in $(seq 1 "$steps"); do
        "$tool_xdotool" mousemove_relative -- $((dx / steps)) $((dy / steps))
        sleep 0.1
    done
    "$tool_xdotool" mouseup 1
    "$tool_xdotool" mousemove 5 5
    sleep 2
}

case "$scenario" in
    radar) ;;
    map) click 240 28 ;;
    measure)
        click 240 28
        click 72 387
        click 420 300
        click 700 320
        click 690 560
        click 430 540
        ;;
    pan) drag 240 0 ;;
    zoom) scroll 10 ;;
    small)
        "$tool_xdotool" windowsize "$window" 940 680
        sleep 3
        ;;
    *) fail "unknown scenario $scenario" ;;
esac

geometry="$("$tool_xdotool" getwindowgeometry --shell "$window")"
width="$(printf '%s\n' "$geometry" | sed -n 's/^WIDTH=//p')"
height="$(printf '%s\n' "$geometry" | sed -n 's/^HEIGHT=//p')"

set -o pipefail
"$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$png" ||
    fail "the framebuffer could not be captured"

kill -0 "$app_pid" 2>/dev/null || fail "the application exited while being driven"
if grep -q 'panicked at' "$log"; then
    fail "the application panicked; see $log"
fi

# A blank or single-colour frame is a failed render even when the process is
# alive. Counting distinct colours rejects it without needing a golden image,
# whose contents depend on live provider data.
for tool in pnmscale pnmtoplainpnm; do
    found="$(find_tool "$tool")" || fail "$tool is required and was not found"
    printf -v "scale_$tool" '%s' "$found"
done
colours="$("$tool_pngtopnm" "$png" 2>/dev/null |
    "$scale_pnmscale" -xsize 200 -ysize 125 2>/dev/null |
    "$scale_pnmtoplainpnm" 2>/dev/null |
    awk 'NR <= 3 { next } { for (i = 1; i <= NF; i++) { seen = seen + 1; part[seen % 3] = $i; if (seen % 3 == 0) print part[0] "," part[1] "," part[2] } }' |
    sort -u | wc -l)"
if [[ -z "$colours" ]] || ((colours < 16)); then
    fail "the captured frame has $colours distinct colours, which is not a rendered interface"
fi

if [[ "$scenario" == "measure" ]]; then
    # The measurement stroke is drawn in the fixed stale-amber colour, which the
    # ambient map chrome barely uses, so a painted polygon stands out in the
    # count. The map region is excluded of the rail and the forecast panel.
    stroked="$("$tool_pngtopnm" "$png" 2>/dev/null |
        "$tool_pamcut" -left 320 -top 56 -width 660 -height 700 2>/dev/null |
        "$tool_pnmtoplainpnm" 2>/dev/null |
        awk 'NR <= 3 { next }
             { for (i = 1; i <= NF; i++) { k = n % 3; part[k] = $i; n++
                 if (k == 2 && part[0] == 255 && part[1] == 196 && part[2] == 64) c++ } }
             END { print c + 0 }')"
    if [[ -z "$stroked" ]] || ((stroked < 400)); then
        fail "the measured polygon is not on the map ($stroked stroke pixels)"
    fi
    printf 'gui: measurement drew %s stroke pixels\n' "$stroked"
fi

printf 'gui: %s ok (%sx%s, %s colours) %s\n' "$scenario" "$width" "$height" "$colours" "$png"
