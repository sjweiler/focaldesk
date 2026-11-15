# Known Issues

FocalDesk is alpha software. Keep another desktop session installed and do not
use FocalDesk as the only environment protecting important work.

- DRM/KMS, HDR, suspend/resume, hardware cursors, direct scanout, multi-GPU,
  hotplug, mixed refresh rates, scaling, and transforms remain hardware- and
  driver-dependent.
- Multi-GPU rendering is supported on raw Vulkan and validated with RTX 4090
  primary outputs plus an Intel UHD 770 companion. Secondary outputs use SDR
  and software cursors; incompatible client buffers retain a copy fallback.
  The tested GTK companion needs the documented Intel renderer override.
  Sporadic individual TCL blanking was reported at 165 Hz; 120 Hz is the current
  baseline and the cause remains unresolved. See
  [multi-GPU setup and hardware findings](multi-gpu-vulkan.md).
- Portal/PipeWire capture is experimental. The raw Vulkan compatibility
  readback path is capped at 5 FPS to protect desktop responsiveness; use the
  GLES backend for OBS until Vulkan DMA-BUF portal export lands. Wide-gamut SDR capture currently
  requires the documented downstream OBS and xdg-desktop-portal-wlr patches.
- Remote desktop is loopback-only, single-output, single-client, and view-only.
  It uses short-lived development credentials and has not yet completed the
  two-client-implementation interoperability gate.
- Theme gradients are currently limited to eight ordered stops and render as
  the primary desktop-background paint behind the wallpaper.
- XWayland works for tested applications, including selected Wine/DXVK setups,
  but broad game and toolkit compatibility is not guaranteed.
- The native Settings fallback panel still contains placeholder pages. The GTK
  Settings application is the supported configuration interface.
- FocalDesk does not yet promise stable APIs or configuration compatibility
  across alpha releases. Legacy `config.toml` is retained as a recovery copy
  after automatic migration to `settings.json`.

When reporting an issue, attach the bounded archive produced by
`focaldesk-cli diagnostics` and include the GPU, driver, backend, output
topology, and exact reproduction steps.
