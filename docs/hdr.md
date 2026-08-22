# HDR Support

FocalDesk includes experimental HDR and color-management paths for HDR10-capable
displays. Support depends on the GPU, driver, connector, display, and output
topology; an HDR-capable monitor alone is not sufficient.

Current work includes HDR rendering, output encoding, color metadata, and SDR
content handling. These paths are under active development and should not be
treated as color-accurate or production-ready without measurement on the target
hardware.

## Known working configuration

As of August 20, 2026, HDR10 has been observed working in a native FocalDesk
session on Fedora 44 with the proprietary NVIDIA 595 driver series, including
HDR content in Google Chrome. This is a successful compatibility result, not a
blanket support guarantee: the GPU model, display, connector, mode, kernel,
Chrome version, FocalDesk revision, and single- or multi-output topology can all
affect the result.

For a reproducible compatibility report, record those details together with the
driver's complete version and whether Chrome was running as a native Wayland or
XWayland client. See [Compatibility Testing](compatibility-testing.md#hardware-result-record).

The bundled wallpaper is re-graded during FP16 composition rather than converted
to a nominal 10-bit asset. Its SDR texture remains the common source for SDR,
wide-gamut SDR, and HDR10. HDR10 composition keeps diffuse wallpaper at the
203-nit BT.2408 reference white, then applies a modest lift to isolated stars,
cyan/orange accents, and the planet rim inside the panel's real headroom. That
authored ceiling is about 450 nits on DisplayHDR 400-class VA panels: they
cannot reproduce 800–1000 nit spectacle, and inventing it only clips or trips
the monitor's global tone map. The dark space background stays at its original
black floor. The final output stage still owns BT.2020 conversion and ST 2084
encoding. KMS HDR static metadata signals 203-nit BT.2408 SDR/graphics white
as MaxFALL and 450 nits as MaxCLL / mastering peak, rather than the Type-1
EDID values (often 409 / ~300) that make these ASUS panels over-compress.

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

Apply both patches and compile clean matching source trees with:

```sh
just install-wide-gamut-capture-build-deps
just build-wide-gamut-capture
```

The dependency recipe targets Fedora and requires `sudo`. The build recipe
itself does not require root. By default it fetches the tested releases below
`target/wide-gamut-capture` and creates a `build-focaldesk` directory in each
source tree. Existing clean source trees can instead be supplied as the two
recipe arguments. The recipe does not install the resulting binaries. To apply
the patches without building, use:

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

## Monitor color gamut

Each monitor has an independent **Color gamut** setting in Display Settings:

- **Auto** uses the resolved ICC/EDID monitor profile.
- **sRGB** constrains SDR output and wide-gamut SDR content to sRGB.
- **Display P3** uses Display P3 as the SDR output space.

This setting does not replace the output dynamic-range choice. When HDR10 is
active, scanout is always encoded and signaled as 10-bit BT.2020 with ST 2084
PQ. Tagged sRGB and Display P3 surfaces are decoded into the scene-linear FP16
working buffer, converted from scene Rec.709 primaries to BT.2020, and only then
PQ encoded.

Chromium's HDR10 path copies the compositor preferred description onto the
window. HDR Chrome sessions must not use `--force-color-profile=scrgb-linear`:
that profile's BT.709 primaries make Blink report `color-gamut: srgb`, so
wide-gamut CSS such as the new-tab shortcut circles disappears. SDR and
linear-SDR sessions keep `--force-color-profile=display-p3-d65`. HDR10 scanout
stays BT.2020/PQ. The preferred description for P3-class panels is Display P3
with extended sRGB (Chrome's HDR raster: 1.0 is paper white). Advertising PQ
on 8-bit windows made the W-test reds clip together and broke still shading.
Named BT.2020 panels still prefer BT.2020+PQ. Blink reports `color-gamut: p3`
for HDR, not `rec2020`. PQ encode maps the scene into Display P3 D65 then
BT.2020. `ext_srgb` is still advertised so clients that actually use extended
sRGB can tag it directly. 8-bit `AB24` is decoded as Display P3 sRGB-HDR, not
ST.2084; 10-bit packed RGB can keep PQ. Decode dithers those 8-bit
sRGB-HDR samples so video skies do not stair-step into purple and blue
slabs; it cannot invent the missing codes. FP16 surfaces tagged PQ decode as
Rec.709 linear HDR. linux-dmabuf feedback prefers FP16 and 10-bit RGB so
Chrome can upgrade off 8-bit. The factory ICC white is often warm and is not
used for HDR. PQ scanout tone-maps highlight luminance into about 450 nits of
DisplayHDR 400-class headroom instead of clipping R, G, and B independently,
so near-white clouds stay white instead of turning magenta or cyan. Channel
values may exceed that luminance peak: Rec.2020 blue's Y weight is only
0.0593, and treating 450 nits as a per-channel cap crushes sky and UI blues.

Live NVIDIA KMS HDR transitions remain guarded because testing with older driver
and system combinations produced GPU Xid 56 faults, flip-event timeouts, and
frozen scanout. The successful Fedora 44/NVIDIA 595 result above is encouraging,
but does not establish that every GPU and output topology is safe. Atomic commit
success and connector-property readback did not reliably indicate that the
display pipeline survived on the affected combinations. NVIDIA therefore still
requires `FOCALDESK_HDR_ALLOW_NVIDIA=1`. Normal and multi-output topologies
additionally require `FOCALDESK_HDR_NVIDIA_DUAL=1`; without both overrides, the
compositor leaves the requested outputs in SDR.

For a guarded NVIDIA multi-output test, configure both overrides in the session
environment:

```toml
[session_environment]
FOCALDESK_HDR_ALLOW_NVIDIA = "1"
FOCALDESK_HDR_NVIDIA_DUAL = "1"
```

Do not set `FOCALDESK_EXCLUSIVE_HDR_OUTPUT` for this test. Remove
`FOCALDESK_HDR_OUTPUT` as well to permit every HDR-requested connector, or set it
to one exact connector to test one HDR output while the other outputs stay in
SDR. The frame watchdog, connector-property validation, persisted request
disable, and SDR rollback remain active.

Display Settings also provides **Apply Requested HDR10** under **Experimental
HDR10**. Enable **HDR output request** on each display that should use HDR10,
then use the red apply button. Requests are independent: capable requested
outputs use BT.2020/PQ while unrequested outputs remain SDR. Mixed HDR10 and
SDR+ICC will not visually match by default: the HDR head is D65 PQ while the SDR
head keeps its ICC white point, and Chrome switches to scRGB whenever any output
has live HDR. The compositor still applies the appropriate per-output transform.
`FOCALDESK_HDR_OUTPUT` and **Restart & Try HDR10** remain the one-head test paths.
The output topology is left unchanged.

A failed exclusive HDR attempt blocks only automatic exclusive retry. It does
not persist-disable that connector's ordinary `hdr_requested` flag.

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

### Session-start exclusive HDR mode

In Display Settings, expand an HDR-capable monitor and press **Restart & Try
HDR10** under **Experimental exclusive HDR10**. The destructive action records
a one-shot request and logs out the current session. Administrators can select
the same exact connector in the session environment:

```toml
[session_environment]
FOCALDESK_EXCLUSIVE_HDR_OUTPUT = "DP-3"
```

This experimental mode is applied only while the DRM backend starts or rebuilds
after a connector hotplug. It automatically requests HDR on the selected output;
the ordinary Display Settings HDR switch does not also need to be enabled. The
exclusive selector takes precedence over `FOCALDESK_HDR_OUTPUT` when both are
present.

Before disabling another output, FocalDesk requires the selected connector to
be connected, have a mode and usable CRTC, expose the HDR10 connector controls
and EDID metadata, permit 10-bit link depth, and pass the driver safety policy.
It then initializes that connector first and verifies the HDR working and
scanout formats. If either preflight fails, all connected outputs remain active
and the exclusive selector prevents HDR from being attempted on a different
connector.

All outputs use their preferred/native resolution at the fastest advertised
refresh no greater than 120 Hz. Keeping the SDR and HDR timings identical avoids
adding a refresh-rate modeset to HDR transitions, while the conservative ceiling
also avoids selecting a timing that fits at 8 bpc but exceeds the connector's
payload budget when HDR requires a 10-bpc link.

Exclusive mode attaches BT.2020, HDR10 metadata, and any available 10-bpc link
control to Smithay's pending connector state before the first real scanout
commit. The first framebuffer is PQ encoded, so the initial atomic modeset
contains the mode, connector, 10-bit primary plane, HDR properties, and PQ
framebuffer together. No baseline SDR frame is submitted on the exclusive
output. After that initial commit passes connector-property readback, the output
remains in a **Verifying** state for at least five seconds and 300 successfully
submitted PQ frames. FocalDesk reports HDR active only after both gates complete.

An encode failure, property mismatch, failed frame, or watchdog timeout records
the attempt as failed, rolls KMS back to SDR when the GPU is still responsive,
and rebuilds the ordinary all-output topology. The state lives at
`$XDG_STATE_HOME/focaldesk/exclusive-hdr.json`. A failed exclusive attempt
blocks only automatic exclusive retry. It does not clear a saved `hdr_requested`
preference on that connector. **Apply Requested HDR10** and dual-head NVIDIA HDR
still apply the saved per-output requests on the next login. If exclusive HDR was
already verified Active, logout, restart, and shutdown re-arm that request so
the next session can enter HDR10 again. An unfinished Starting/Verifying
attempt still fail-safes to SDR after a crash. Display Settings shows the saved
failure reason and allows an explicit exclusive retry.

On NVIDIA, exclusive mode additionally requires:

```toml
FOCALDESK_HDR_ALLOW_NVIDIA = "1"
```

By itself this does not permit NVIDIA HDR with multiple active outputs; that
also requires `FOCALDESK_HDR_NVIDIA_DUAL=1` as described above. A complete GPU
wedge can prevent in-session recovery, so keep the text-console recovery path
below available during testing.

This is a restart-backed Windows-style topology switch, not a live KMS switch.
Use **Disable & Restart** after HDR verifies active to restore the normal
multi-monitor topology. The button's disabled state overrides a persistent
`FOCALDESK_EXCLUSIVE_HDR_OUTPUT` environment selection.

Capable outputs prefer a 10-bit scanout format during initialization and keep
that format for SDR and HDR. Ordinary live HDR toggles require three
successfully submitted baseline SDR frames before changing the connector's
BT.2020 colorspace, HDR metadata, PQ rendering, and `max bpc` when that connector
property is available. Exclusive startup mode instead includes those properties
in the initial modeset and submits PQ from its first real frame. Neither path
changes the selected scanout buffer format after initialization. Drivers that
manage link depth without a `max bpc` property must initialize a 10-bit KMS
scanout format.
After the completion event, FocalDesk reads the connector properties back and
only reports HDR active when the colorspace is BT.2020, the metadata blob is
active, and any exposed `max bpc` property reads at least 10. A partial or
unverifiable transition clears the saved HDR request and stages an SDR rollback.

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
