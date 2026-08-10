# HDR Support

FocalDesk includes experimental HDR and color-management paths for HDR10-capable
displays. Support depends on the GPU, driver, connector, display, and output
topology; an HDR-capable monitor alone is not sufficient.

Current work includes HDR rendering, output encoding, color metadata, and SDR
content handling. These paths are under active development and should not be
treated as color-accurate or production-ready without measurement on the target
hardware.

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
