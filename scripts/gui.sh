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

# A scenario that only has something to show over snow, sea ice, or a mountain
# range starts from a seeded place: OXRAD_GUI_PLACE="46.5 10.0 Alps".
if [[ -n "${OXRAD_GUI_PLACE:-}" ]]; then
    read -r seed_lat seed_lon seed_name <<< "$OXRAD_GUI_PLACE"
    mkdir -p "$home/.oxrad"
    {
        printf 'place_name = "%s"\n' "$seed_name"
        printf 'place_region = "Somewhere"\n'
        printf 'place_country = "Earth"\n'
        printf 'place_country_code = "xx"\n'
        printf 'place_lat = "%s"\n' "$seed_lat"
        printf 'place_lon = "%s"\n' "$seed_lon"
    } > "$home/.oxrad/config.toml"
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
zoom_out_at() {
    local steps="$1" x="$2" y="$3"
    "$tool_xdotool" mousemove "$x" "$y"
    for _ in $(seq 1 "$steps"); do
        "$tool_xdotool" click 5
        sleep 0.2
    done
    sleep 2
}
zoom_in_at() {
    local steps="$1" x="$2" y="$3"
    "$tool_xdotool" mousemove "$x" "$y"
    for _ in $(seq 1 "$steps"); do
        "$tool_xdotool" click 4
        sleep 0.2
    done
    sleep 2
}
rail_scroll() {
    local steps="$1"
    "$tool_xdotool" mousemove 150 500
    for _ in $(seq 1 "$steps"); do
        "$tool_xdotool" click 5
        sleep 0.15
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
    satellite)
        click 240 28
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/satellite-before.png"
        click 46 465
        sleep "${OXRAD_GUI_SATELLITE_SETTLE:-12}"
        ;;
    rivers)
        click 240 28
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/rivers-before.png"
        click 60 707
        sleep "${OXRAD_GUI_RIVERS_SETTLE:-14}"
        rail_scroll 6
        ;;
    space)
        click 240 28
        rail_scroll 10
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/space-before.png"
        click 60 392
        sleep "${OXRAD_GUI_SPACE_SETTLE:-12}"
        ;;
    air)
        click 240 28
        rail_scroll 18
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/air-before.png"
        click 60 372
        sleep "${OXRAD_GUI_AIR_SETTLE:-12}"
        ;;
    climate)
        click 240 28
        rail_scroll 22
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/climate-before.png"
        click 60 374
        sleep "${OXRAD_GUI_CLIMATE_SETTLE:-24}"
        ;;
    fires)
        click 240 28
        rail_scroll 26
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/fires-before.png"
        click 60 373
        sleep "${OXRAD_GUI_FIRES_SETTLE:-14}"
        ;;
    environment)
        click 240 28
        rail_scroll 30
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/environment-before.png"
        click 70 373
        sleep "${OXRAD_GUI_ENVIRONMENT_SETTLE:-12}"
        ;;
    ocean)
        click 240 28
        zoom_out_at 9 700 350
        zoom_in_at 9 430 430
        rail_scroll 34
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/ocean-before.png"
        click 60 372
        sleep "${OXRAD_GUI_OCEAN_SETTLE:-14}"
        ;;
    observations)
        click 240 28
        rail_scroll 40
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/observations-before.png"
        click 60 375
        sleep "${OXRAD_GUI_OBSERVATIONS_SETTLE:-12}"
        ;;
    snow)
        # The rail is scrolled to its end, where the snow section keeps a fixed
        # distance from the alert list below it whatever arrives above.
        click 240 28
        rail_scroll 60
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/snow-before.png"
        click 60 375
        sleep "${OXRAD_GUI_SNOW_SETTLE:-14}"
        ;;
    profile)
        # The same end-of-rail scroll puts the vertical-profile section one
        # section above the alert list.
        click 240 28
        rail_scroll 60
        "$tool_xwdtopnm" "$output/Xvfb_screen0" | "$tool_pnmtopng" > "$output/profile-before.png"
        click 60 375
        sleep "${OXRAD_GUI_PROFILE_SETTLE:-14}"
        rail_scroll 10
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

if [[ "$scenario" == "satellite" ]]; then
    # Turning the layer on must change the map itself, not merely a status
    # line. Both frames are cut to the map canvas and reduced before the
    # comparison, so provider imagery that failed to arrive leaves the region
    # almost identical and the run fails.
    sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 320 -top 120 -width 660 -height 500 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 120 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(sample "$output/satellite-before.png") <(sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 2000)); then
        fail "the satellite layer changed $changed map samples; imagery never arrived"
    fi
    printf 'gui: satellite imagery redrew %s map samples\n' "$changed"
fi

if [[ "$scenario" == "rivers" ]]; then
    # The rail below the imagery controls is where the discharge section and its
    # hydrograph appear, so a layer that fails to fetch leaves that strip alone.
    rail_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 300 -width 320 -height 420 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 210 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(rail_sample "$output/rivers-before.png") <(rail_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 800)); then
        fail "the river section changed $changed rail samples; the hydrograph never arrived"
    fi
    printf 'gui: river section redrew %s rail samples\n' "$changed"
fi

if [[ "$scenario" == "space" ]]; then
    # The Kp chart is drawn under the toggle, so an index that never arrives
    # leaves that strip exactly as it was.
    space_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 330 -width 320 -height 260 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 130 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(space_sample "$output/space-before.png") <(space_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 500)); then
        fail "the geomagnetic section changed $changed rail samples; the Kp chart never arrived"
    fi
    printf 'gui: geomagnetic section redrew %s rail samples\n' "$changed"
fi

if [[ "$scenario" == "air" ]]; then
    # The index chart sits under the toggle; a record that never arrives leaves
    # the strip unchanged.
    air_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 320 -width 320 -height 250 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 125 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(air_sample "$output/air-before.png") <(air_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 500)); then
        fail "the air-quality section changed $changed rail samples; the index chart never arrived"
    fi
    printf 'gui: air-quality section redrew %s rail samples\n' "$changed"
fi

if [[ "$scenario" == "climate" ]]; then
    # Decades of reanalysis take longer to arrive than a single request, so the
    # wait is longer; the strip is unchanged until the record lands.
    climate_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 330 -width 320 -height 250 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 125 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(climate_sample "$output/climate-before.png") <(climate_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 500)); then
        fail "the climate section changed $changed rail samples; the reanalysis never arrived"
    fi
    printf 'gui: climate section redrew %s rail samples\n' "$changed"
fi

if [[ "$scenario" == "fires" ]]; then
    # The list of nearest perimeters only exists once the service has answered.
    fires_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 330 -width 320 -height 250 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 125 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(fires_sample "$output/fires-before.png") <(fires_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 500)); then
        fail "the wildfire section changed $changed rail samples; no perimeters arrived"
    fi
    printf 'gui: wildfire section redrew %s rail samples\n' "$changed"
fi

if [[ "$scenario" == "environment" ]]; then
    # The energy chart and readout only exist once the fields have arrived.
    environment_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 330 -width 320 -height 250 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 125 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(environment_sample "$output/environment-before.png") <(environment_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 500)); then
        fail "the storm-environment section changed $changed rail samples; no fields arrived"
    fi
    printf 'gui: storm-environment section redrew %s rail samples\n' "$changed"
fi

if [[ "$scenario" == "ocean" ]]; then
    # The scenario ends over open Pacific water, so the section has to show a
    # sea state rather than the inland message.
    ocean_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 330 -width 320 -height 250 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 125 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(ocean_sample "$output/ocean-before.png") <(ocean_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 500)); then
        fail "the ocean section changed $changed rail samples; no sea state arrived"
    fi
    printf 'gui: ocean section redrew %s rail samples\n' "$changed"
fi

if [[ "$scenario" == "observations" ]]; then
    # The station list only exists once the feed has answered.
    observations_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 330 -width 320 -height 250 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 125 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(observations_sample "$output/observations-before.png") <(observations_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 500)); then
        fail "the surface-observation section changed $changed rail samples; no stations arrived"
    fi
    printf 'gui: surface-observation section redrew %s rail samples\n' "$changed"
fi

if [[ "$scenario" == "snow" ]]; then
    # The readout and the snowfall chart replace the one-line invitation, so a
    # layer that never reached the provider leaves this strip unchanged.
    snow_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 380 -width 320 -height 260 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 130 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(snow_sample "$output/snow-before.png") <(snow_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 500)); then
        fail "the snow section changed $changed rail samples; the snow model never answered"
    fi
    printf 'gui: snow section redrew %s rail samples\n' "$changed"
fi

if [[ "$scenario" == "profile" ]]; then
    # The readout, the skewed chart, and the hodograph replace the one-line
    # invitation, so a column that never arrived leaves this strip unchanged.
    profile_sample() {
        "$tool_pngtopnm" "$1" 2>/dev/null |
            "$tool_pamcut" -left 0 -top 300 -width 320 -height 220 2>/dev/null |
            "$scale_pnmscale" -xsize 160 -ysize 110 2>/dev/null |
            "$scale_pnmtoplainpnm" 2>/dev/null |
            awk 'NR <= 3 { next } { printf "%s %s %s\n", $1, $2, $3 }'
    }
    changed="$(diff <(profile_sample "$output/profile-before.png") <(profile_sample "$png") |
        grep -c '^[<>]' || true)"
    if [[ -z "$changed" ]] || ((changed < 500)); then
        fail "the profile section changed $changed rail samples; the column never arrived"
    fi
    printf 'gui: profile section redrew %s rail samples\n' "$changed"
fi

printf 'gui: %s ok (%sx%s, %s colours) %s\n' "$scenario" "$width" "$height" "$colours" "$png"
