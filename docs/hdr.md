# HDR Support

FocalDesk includes experimental HDR and color-management paths for HDR10-capable
displays. Support depends on the GPU, driver, connector, display, and output
topology; an HDR-capable monitor alone is not sufficient.

Current work includes HDR rendering, output encoding, color metadata, and SDR
content handling. These paths are under active development and should not be
treated as color-accurate or production-ready without measurement on the target
hardware.

## SDR wide gamut and 10-bit output

HDR is not required to benefit from FocalDesk's color pipeline. An SDR output
can use a monitor ICC profile, accept color-managed wide-gamut client content,
compose in a floating-point scene buffer, and encode the completed frame for
the monitor. This preserves colors outside sRGB when the application, profile,
GPU, connector, and display all support them; it does not add HDR brightness or
HDR metadata.

Wide gamut and color depth describe different improvements:

- Wide gamut increases the range of colors that the pipeline can represent.
- A conventional 8-bit RGB output has 256 values per channel, or 16,777,216
  encoded RGB combinations. A 10-bit RGB output has 1,024 values per channel,
  or 1,073,741,824 combinations. The extra precision reduces visible banding
  in gradients, but it does not make every application buffer 10-bit or imply
  that a panel can visibly distinguish every combination.

Capable outputs prefer a 10-bit scanout format even while HDR is disabled, so
SDR wide-gamut color and the additional output precision can be used together.
The complete chain still matters: a browser test can confirm that a client and
compositor negotiated wide-gamut content, but it does not by itself prove the
active scanout format or identify the ICC profile used for output encoding.

Choose an ICC file independently for each connector in Display Settings. The
selection is saved in `settings.json` and `displays.json`; restart the FocalDesk
session after changing it so the compositor loads and bakes the new per-output
ICC LUT. The display summary reports whether wide gamut is active and whether
the ICC LUT fell back to the parametric path.

FocalDesk owns color conversion in its Wayland compositor. `colormgr` reports
the separate `colord` database and may show profiles that FocalDesk is not
using, particularly when two similar monitors are connected. For runtime
diagnosis, prefer the FocalDesk display summary and compositor log. A successful
load records the output ID, monitor serial, effective primaries, ICC byte count,
and LUT size; a failed load or encode records an explicit ICC fallback warning.

## Wide-gamut SDR recording with OBS

The stock portal/OBS path remains untagged sRGB/Rec.709. FocalDesk also has an
opt-in BT.2020 SDR capture contract for a patched capture stack:

```toml
[session_environment]
FOCALDESK_PORTAL_COLOR = "bt2020-sdr"
```

This mode converts the FP16 compositor scene to BT.2020 with the BT.709 SDR
transfer function and exposes only 10-bit RGB DMA-BUF capture formats. It does
not fall back to 8-bit or SHM, because either would silently lose precision.
The compositor also publishes the setting to portal services and logs the
active contract and advertised formats at startup.

The matching integration patches are versioned with the components tested by
FocalDesk:

- `patches/xdg-desktop-portal-wlr-0.8.1-bt2020-sdr.patch` attaches full-range
  RGB, BT.709 transfer, and BT.2020-primary colorimetry to the PipeWire stream.
- `patches/obs-studio-32.1.1-bt2020-sdr.patch` consumes that colorimetry,
  converts the captured texture into OBS's linear working space, adds a
  **Rec. 2020 (SDR)** canvas option, supports I010/P010 conversion, and writes
  BT.2020/BT.709/BT.2020-NCL encoder and container metadata.

Apply both patches to clean matching source trees with:

```sh
scripts/apply-wide-gamut-capture-patches.sh \
  /path/to/obs-studio-32.1.1 \
  /path/to/xdg-desktop-portal-wlr-0.8.1
```

After building and installing both, restart the FocalDesk session. In OBS,
select **P010** (or I010), **Rec. 2020 (SDR)**, and a 10-bit HEVC or AV1
recording encoder. Do not select Rec. 2100 PQ/HLG; this contract is SDR.
Validate a recording with `ffprobe`: the pixel format must be 10-bit and its
primaries, transfer, and matrix must report BT.2020, BT.709, and BT.2020-NCL.

The longer-term goals are:

- HDR scanout
- HDR rendering
- SDR compatibility
- Future native HDR Wayland applications
- Color-managed desktop rendering

NVIDIA and multi-output paths have additional safeguards because some
combinations have caused failed or frozen atomic commits during development.
Diagnostic overrides exist in the codebase, but they are intentionally not
recommended as general user configuration.

For a controlled multi-monitor test, enable HDR for the intended display in
Display Settings and select its connector in the compositor environment. When
the native `focaldmd` greeter launches FocalDesk, add this to
`/etc/focaldmd.toml`:

```toml
[session_environment]
FOCALDESK_HDR_OUTPUT = "DP-3"
```

Then restart `focaldmd` or reboot before logging in again. For a compositor
started directly from a shell, the equivalent is:

```sh
FOCALDESK_HDR_OUTPUT=DP-3 focaldesk-desktop
```

`FOCALDESK_HDR_OUTPUT` is an exact, single-connector safety filter for both the
PQ render path and live KMS HDR changes. It does not enable HDR by itself or
modify the saved display preference. Find connector names in `displays.json` or
the compositor's output-detection log. An unknown name selects no output.

Capable outputs prefer a 10-bit scanout format during initialization and keep
that format for SDR and HDR. The HDR toggle changes PQ rendering, BT.2020
colorspace, and metadata together on one frame; it does not change the live
scanout format or force the connector's `max bpc` property.

## Recovering from a frozen HDR session

The DRM watchdog attempts to recover a stalled HDR commit after two seconds and
clears the saved HDR request for that output. A driver-wide GPU lock can also
stall the watchdog, so keep an out-of-session recovery path available while
testing:

1. Press `Ctrl+Alt+F2` to switch away from FocalDesk and log in on the text
   console.
2. Edit `/etc/focaldmd.toml` and set these values in its existing
   `[session_environment]` table:

   ```toml
   FOCALDESK_HDR = "0"
   ```

3. Run `sudo systemctl restart focaldmd`. The next login uses SDR even when an
   output still has a saved HDR preference.
4. Disable HDR for the affected output in Display Settings, then remove the
   temporary override and restart `focaldmd` again.

Only declare `[session_environment]` once in the TOML file. If switching virtual
terminals is also unresponsive, reboot into a text or rescue target and apply
the same overrides before starting `focaldmd`.

When reporting an HDR problem, include the GPU and driver version, connector
names, display models, modes and refresh rates, whether more than one output was
active, and the relevant FocalDesk log excerpt. See
[Troubleshooting](troubleshooting.md).
