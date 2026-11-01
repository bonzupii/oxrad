# Headless graphical verification

The following workflow exercised the release binary on Ubuntu 24.04 x86-64 with Xvfb and Mesa lavapipe. It uses a private X display with access control disabled. Run it only in an isolated environment where other users cannot connect to that display. The captured frames contain live provider data, so their contents change over time. Screenshots establish rendering and layout evidence, not meteorological correctness or complete interaction coverage.

The restored workspace contained Xvfb, `xkbcomp`, Mesa's `libvulkan_lvp.so`, and their runtime libraries under `.tools/root`. The host provided XKB configuration under `/usr/share/X11/xkb`. The test additionally used Ubuntu's `proot`, `libtalloc2`, `netpbm`, `libnetpbm11t64`, `xdotool`, `libxdo3`, and `libxtst6`, extracted into `.tools/root`. Their package metadata and checksums are available at https://packages.ubuntu.com/noble/. Install these components locally when they are absent; `.tools` is ignored by Git. Xvfb's hard-coded `/usr/bin/xkbcomp` path is mapped to the local executable with PRoot. The application uses WGPU's Vulkan backend and the local lavapipe driver; it does not run against a simulated window.

Create `.tools/lvp-workspace.json` with the absolute path to the local driver, replacing `WORKSPACE` with the absolute project directory:

```json
{
  "ICD": {
    "api_version": "1.1.274",
    "library_path": "WORKSPACE/.tools/root/usr/lib/x86_64-linux-gnu/libvulkan_lvp.so"
  },
  "file_format_version": "1.0.0"
}
```

Build and start the display and application from the project directory. Pick an unused display number. The isolated `HOME` keeps the test's configuration and downloaded cache separate from a user's normal OXRAD data.

```sh
cargo build --release --locked
mkdir -p target/local-x11 target/headless-home artifacts
export LD_LIBRARY_PATH="$PWD/.tools/root/usr/lib/x86_64-linux-gnu"
.tools/root/usr/bin/proot -b "$PWD/.tools/root/usr/bin/xkbcomp:/usr/bin/xkbcomp" .tools/root/usr/bin/Xvfb :91 -screen 0 1280x800x24 -nolisten tcp -ac -fbdir "$PWD/target/local-x11" > target/local-x11/xvfb.log 2>&1 < /dev/null &
echo $! > target/local-x11/xvfb.pid
export DISPLAY=:91 VK_DRIVER_FILES="$PWD/.tools/lvp-workspace.json" WGPU_BACKEND=vulkan HOME="$PWD/target/headless-home"
target/release/oxrad --site KTWX > target/local-x11/oxrad.log 2>&1 < /dev/null &
echo $! > target/local-x11/oxrad.pid
```

Confirm that both recorded processes remain alive and inspect both logs. XKB warnings about unrecognized keysyms appeared in this environment without preventing the server from starting. A running process alone does not establish a usable display.

Use `xdotool search --name oxrad` to find the application window and `xdotool windowsize WINDOW 1040 640` to inspect the minimum size. The map tab, history controls, and other application controls can be exercised with XTEST events through `xdotool`. Capture the actual Xvfb framebuffer as a PNG:

```sh
set -o pipefail
.tools/root/usr/bin/xwdtopnm target/local-x11/Xvfb_screen0 | .tools/root/usr/bin/pnmtopng > artifacts/workstation.png
```

Inspect the resulting PNG and application log. A framebuffer capture is the whole virtual screen, so a resized window occupies only part of the image. Stop only the recorded application and Xvfb processes after the test.

## Assertion harness

`scripts/gui.sh <scenario>` performs the launch, input, capture, and checks above and exits non-zero when any of them fail. It finds its tools on `PATH` and then under `.tools/root/usr/bin`, and reports a missing tool as a failure rather than skipping.

Scenarios: `radar`, `map`, `measure`, `satellite`, `rivers`, `pan`, `zoom`, `small`. Each one asserts that the window appeared, that the process was still alive after the input, that the log contains no panic, and that the captured frame holds at least 16 distinct colours after downscaling to 200×125. A blank or single-colour frame fails. The `measure` scenario additionally clicks four corners on the map and requires at least 400 pixels drawn in the measurement stroke colour inside the map region, so a measurement that stops painting fails the run. The `satellite` scenario captures the map before and after turning the NASA imagery layer on and requires at least 2000 of its downsampled map samples to change, so a layer that reports tiles but paints nothing fails the run. The `rivers` scenario turns the discharge layer on, scrolls the rail to the hydrograph, and requires at least 800 rail samples to change against the same strip before the layer was on, so a record that never arrives fails the run. Both read live provider data, so they need network access. Screenshots and logs land in `target/gui/`.

```sh
cargo build --release --locked
scripts/gui.sh radar
scripts/gui.sh map
scripts/gui.sh measure
scripts/gui.sh pan
scripts/gui.sh small
```

The colour count rejects an empty frame without a golden image, whose contents depend on live provider data. It does not establish that any particular element is drawn in the right place; georeferencing is covered by the placement tests, including the shader test that parses the WGSL the pipeline compiles.

The harness was verified to fail as intended: pointing `OXRAD_GUI_BINARY` at a binary that exits immediately yields `gui: the application never opened a window` and exit status 1.

Coverage still missing: warning refresh, history playback, keyboard navigation, every panel, offline operation, and end-to-end user journeys.
