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

When reporting an HDR problem, include the GPU and driver version, connector
names, display models, modes and refresh rates, whether more than one output was
active, and the relevant FocalDesk log excerpt. See
[Troubleshooting](troubleshooting.md).
