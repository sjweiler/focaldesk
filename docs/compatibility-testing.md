# Compatibility Testing

FocalDesk uses a nested smoke test for repeatable compositor checks before
hardware-session testing. The harness runs inside an existing Wayland session
or starts a private headless Weston host when no Wayland display is available.
It does not install or enable desktop services.

## Run the automated smoke test

Install `wayland-info`, Weston, and XWayland, then run:

```sh
just nested-smoke
```

The harness:

1. Builds the winit compositor with XWayland support.
2. Starts a private headless host when necessary.
3. Waits for FocalDesk's client socket.
4. Performs a Wayland registry round-trip and checks core globals.
5. Connects a native demo client when one is installed.
6. Confirms the client exercises precise surface-tree damage and records the
   latest tree/rectangle/fallback counters.
7. Confirms the compositor survives client rendering.
8. Confirms XWayland reaches its ready state.
9. Rejects panic and crash signatures in the logs.

Results are written to `target/nested-smoke` by default. Preserve that directory
when filing a compatibility bug. It contains a short summary, compositor logs,
the advertised Wayland registry, host logs when headless Weston was needed, and
demo-client output.

To reuse an existing build:

```sh
just nested-smoke-no-build
```

To choose a client or artifact directory:

```sh
FOCALDESK_SMOKE_CLIENT="gtk4-demo" \
FOCALDESK_SMOKE_ARTIFACTS=/tmp/focaldesk-smoke \
  bash scripts/nested-smoke.sh --no-build
```

The client command is intended for trusted local test commands. It is observed
for five seconds by default; a timeout is a successful result when the client
remains open and the compositor remains healthy.

## Compatibility matrix

| Area | Nested automation | Manual or hardware follow-up |
| --- | --- | --- |
| Compositor startup and first render | Required | DRM/KMS login session |
| Wayland registry and round-trip | Required | Protocol-specific applications |
| Native XDG client connection and precise damage | Required when a demo client is installed | GTK, Qt, browsers, Electron, games |
| XWayland server startup | Required | Real X11 applications, Wine, clipboard, drag and drop |
| Panic/crash detection | Required | Long-running soak and recovery |
| Window resize, maximize, and fullscreen | Client survival only | Visual and input verification |
| Clipboard and primary selection | Unit coverage | Cross-toolkit interoperability |
| Fractional scaling and transforms | Unit/runtime coverage | Mixed-DPI visual verification |
| Multiple outputs and hotplug | State/unit coverage | Physical connector testing |
| HDR, ICC, direct scanout, and hardware cursor | Unit coverage | Supported GPU/display hardware |
| Portal capture and PipeWire | Unit/configuration coverage | OBS/browser capture session |

Passing the nested test means the build has a working baseline. It is not a
claim of complete application, GPU, display, or protocol compatibility.

## Hardware result record

For each direct-session run, record:

- FocalDesk revision and build profile.
- Distribution, kernel, Mesa, GPU, and driver versions.
- GPU model and whether the system is multi-GPU.
- Connector, mode, refresh rate, scale, transform, HDR request, and ICC profile.
- Native and XWayland applications exercised.
- Login, suspend/resume, lock/unlock, hotplug, and logout results.
- Relevant compositor and journal logs with private data removed.

Use the same record for successes and failures so the supported hardware matrix
can be based on evidence rather than isolated bug reports.
