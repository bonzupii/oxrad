# Headless graphical verification

The graphical smoke test runs the release binary on a private Xvfb display with
Mesa lavapipe and captures the framebuffer. It proves that a frame was drawn
and that the application stayed alive during the driven input. It does not
prove meteorological correctness or complete interaction coverage.

The current evidence, including which frames were inspected and which states
were not captured, is in
[RECONSTRUCTION_STATE.md](RECONSTRUCTION_STATE.md).

## Environment

The documented environment used Xvfb, `xkbcomp`, Mesa `libvulkan_lvp.so`,
`proot`, `netpbm`, `xdotool` and their runtime libraries. `scripts/gui.sh` finds
the tools on `PATH` and then under `.tools/root/usr/bin`, which is ignored by
Git. A restored workspace may need those tools reinstalled; their absence is a
test failure, not a skipped check.

The application uses the Vulkan backend and the local lavapipe driver. It runs
against a real X server, not a simulated window. WGPU software rendering is
slow; the harness waits for the window and a settling period before driving it.

## Run

```sh
cargo build --release --locked
scripts/gui.sh radar
scripts/gui.sh map
scripts/gui.sh measure
scripts/gui.sh pan
scripts/gui.sh small
```

`scripts/gui.sh <scenario> [output-directory]` creates an isolated `HOME`, starts
Xvfb on a display it reports itself, launches the release binary with
`--site KTWX`, finds the window with `xdotool`, and captures the real
framebuffer to a PNG. It stops only the application and Xvfb it started.

A scenario that needs a particular place can seed it:

```sh
OXRAD_GUI_PLACE="46.5 10.0 Alps" scripts/gui.sh snow
```

The wait before the scenario is controlled by `OXRAD_GUI_SETTLE`; the
environmental scenarios also accept their own settle overrides. A shorter wait
is useful for capturing a loading or cancellation frame, but it may capture
before the first provider answers.

## Scenarios

The harness includes radar, map, measure, pan, zoom, small, registry, the
environmental layers, and the `cancel` and `refresh` attempts used to inspect
the transfer controls. The environmental scenarios click a layer, wait for its
provider, and compare a region of the rail or map against a capture taken
before the layer was enabled. A layer that reports success but draws nothing
fails that comparison.

The basic assertions are:

- the window appeared;
- the application remained alive after the input;
- the log contains no panic;
- the captured frame has at least 16 distinct colours after downscaling.

The `measure` scenario additionally counts the measurement stroke colour. The
satellite scenario requires at least 2,000 changed map samples. The rail-layer
scenarios require at least 500 changed rail samples, with a longer wait for the
climate record and the whole-rail scenarios.

A blank or single-colour frame fails. The colour count does not establish that
a particular element is in the right place; georeferencing is covered by the
placement tests.

## Inspect

Open the PNG named for the scenario under `target/gui/` and read the
application log beside it. A running process alone does not establish a usable
interface. Stop only the recorded application and Xvfb processes after a manual
run; the script's trap does this for scripted runs.

## Limits

The provider data is live and changes, so a capture is evidence of one frame.
The harness does not assert text or individual widget states. Cancellation and
refreshing are timing-sensitive: if the transfer completes before the click, or
the revalidation returns before the capture, the frame does not show the
transition. Those attempts are reported as observed or not observed in the
ledger rather than assumed to pass.

Coverage still missing: keyboard navigation, every panel, warning refresh,
history playback, offline operation, cache-budget controls, and complete
end-to-end journeys.
