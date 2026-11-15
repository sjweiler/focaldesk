# Vulkan multi-GPU desktop

The raw Vulkan backend supports opt-in SDR desktop rendering on secondary GPUs.
The primary GPU retains its existing desktop and HDR path. Devices own separate
Vulkan logical devices, DRM devices, GBM scanout buffers, fences, and page-flip
notifiers. Connector/CRTC handles are interpreted only within their owning GPU.

Secondary desktop outputs receive globally unique IDs, join the same Wayland
coordinate space, and use the shared windows, workspaces, clipboard, and input
state. Saved monitor identity, scale, rotation, layout, and role are retained
when a monitor moves to a different GPU/connector. Unconfigured monitors extend
the desktop to the right. The compositor publishes the discovered connector and
resolved role in the display inventory, so the companion can follow a control
panel from an NVIDIA connector to an Intel connector. Secondary rendering currently uses SDR and a software
cursor; HDR remains on the primary path.

## Build and enable

```sh
cargo build --release -p focaldesk-desktop --no-default-features --features drm-vulkan,xwayland
```

Use these variables in the login environment:

```sh
FOCALDESK_DRM_RENDERER=vulkan
FOCALDESK_DRM_SECONDARY_DESKTOP=1
```

For SDDM, add the desktop variable to the installed FocalDesk session's `Exec`
line in `/usr/share/wayland-sessions/focaldesk.desktop`, preserving its existing
renderer and NVIDIA HDR variables. Log out and back in after installation.
Do not start another DRM compositor inside the active graphical session.

`FOCALDESK_DRM_PRIMARY_DEVICE=/dev/dri/by-path/pci-PCI-ADDRESS-card` optionally
selects the primary device using its stable PCI path. Without it, the existing
seat-primary selection is retained. An invalid override fails startup.

The previous `FOCALDESK_DRM_SECONDARY_TEST=1` bring-up mode is still available:
it shows static RGB bars without publishing secondary desktop outputs. Desktop
mode takes precedence when both variables are set. With neither variable set,
secondary devices are only discovered, not opened for KMS rendering.

## Client buffers and copy fallback

The secondary renderer first tries direct DMA-BUF import with its supported
formats/modifiers. Existing renderer synchronization imports client acquire
fences and publishes GPU read-completion fences; client resources remain pinned
until the consuming submission completes. The primary GPU's existing DMA-BUF
feedback and allocation preferences remain unchanged. A shared format/modifier
capability does not establish that an allocation from either driver will import
correctly on the other. The secondary output handles compatibility locally
rather than changing allocations for every dock, rail, and application client.

If a format/modifier is incompatible, or the actual cross-device import fails,
that output switches to a compatibility copy path. The primary GPU renders its
scene to a primary-owned offscreen GBM buffer, asynchronously reads it back, and
the secondary GPU uploads and presents the completed SDR image. It never maps a
tiled client buffer directly on the CPU. Only one copy per output can be in
flight, and copy submissions are limited to 30 per second. New client damage is
retained while a copy is pending. Capture IDs are unique across rebuilds so a
late completion cannot overwrite a newer output generation.

This fallback costs primary-GPU work and CPU/PCIe bandwidth. It is a compatibility
path, not a zero-copy or high-refresh cross-device guarantee. Direct import on
real Intel/NVIDIA driver combinations still requires hardware validation.
Per-surface feedback, DMA-BUF export from secondary client allocations, and
GPU-to-GPU copy optimization remain future improvements.

## Touch input

Touch-down/motion events target a device's libinput output association when
present. `FOCALDESK_TOUCH_OUTPUT=HDMI-A-1` supplies a connector association when
libinput has none. Otherwise, the existing sole `ControlPanel` output is used.
With no control panel, one touchscreen and exactly one secondary output can be
associated automatically. Multiple candidates are not guessed; configure an
explicit association. Coordinates use the target output's origin, scale, and
rotation; libinput's device calibration remains in effect.

The ViewSonic's saved `ControlPanel` role remains intact. It displays the normal
control-panel UI rather than accepting ordinary desktop windows. To test moving
application windows to it, first change its role to a normal display in Settings.

## Recovery and validation

Secondary connector changes and display settings rebuild that GPU's outputs,
retaining desktop topology. Property-only udev events do not repeatedly modeset.
A hung secondary fence or commit is bounded to five seconds; secondary failures
withdraw its outputs and retain the primary desktop. A later connector event
can retry a failed secondary desktop. Device removal unregisters its event
source and closes its libseat ownership. Suspend/resume recreates secondary
Vulkan resources independently. Queue teardown uses a bounded 100 ms drain.

After logging in, verify:

1. `Raw Vulkan multi-GPU: secondary_desktop=true` and
   `Secondary Vulkan desktop presented` appear in the journal.
2. The ViewSonic shows its control-panel UI instead of bars. Both TCL displays
   retain their configured modes and HDR behavior.
3. Touch selects the point touched on ViewSonic, including after rotation or
   scale changes; pointer movement crosses the configured display boundaries.
4. In normal-display role, move SHM and GPU-rendered windows between outputs.
   If copying is required, `Cross-GPU copy enabled` or `Cross-GPU import failed`
   identifies that transition. Check for stale frames and input stalls.
5. Test secondary hotplug, VT switching, suspend/resume, and screenshots.

Compilation and unit tests do not establish physical presentation or touch
accuracy. Validate those behaviors on the target hardware. The pre-login SDDM
rendering path is separate from this compositor code.
