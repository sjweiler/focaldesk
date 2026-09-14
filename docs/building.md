# Building FocalDesk

This guide covers development builds, nested testing, and installation as a
Wayland session. FocalDesk is alpha system software; begin with the nested
backend and keep another desktop session available.

## Supported environments

- Fedora Workstation is the primary development environment.
- Ubuntu is compile-checked by GitHub Actions.
- Rust stable and a Wayland host session are expected.
- Other Linux distributions may work but are not currently tested by the
  project.

Hardware behavior, especially DRM/KMS, HDR, multi-GPU, and capture behavior,
depends on the kernel, Mesa or proprietary driver, and display topology.

## Install prerequisites

Install Rust with [rustup](https://rustup.rs/), then verify the toolchain:

```sh
rustc --version
cargo --version
```

### Fedora

```sh
sudo dnf install \
  meson \
  ninja-build \
  cmake \
  gcc \
  clang \
  libclang-devel \
  pkgconf-pkg-config \
  libxkbcommon-devel \
  wayland-devel \
  wayland-protocols-devel \
  mesa-libEGL-devel \
  mesa-libGLES-devel \
  libdrm-devel \
  libinput-devel \
  seatd-devel \
  libdisplay-info-devel \
  pipewire-devel \
  alsa-lib-devel \
  polkit-devel \
  pam-devel \
  gtk4-devel \
  gtk4-layer-shell-devel \
  libadwaita-devel \
  xorg-x11-server-Xwayland
```

### Ubuntu and Debian-derived distributions

The CI build uses:

```sh
sudo apt update
sudo apt install -y \
  meson \
  ninja-build \
  pkg-config \
  libwayland-dev \
  wayland-protocols \
  libxkbcommon-dev \
  libudev-dev \
  libinput-dev \
  libseat-dev \
  libpipewire-0.3-dev \
  libasound2-dev \
  libpolkit-gobject-1-dev \
  libpolkit-agent-1-dev \
  libclang-dev \
  libegl1-mesa-dev \
  libgles2-mesa-dev \
  libgbm-dev \
  libgtk-4-dev \
  libadwaita-1-dev \
  libpam0g-dev
```

Ubuntu 24.04 does not package the GTK4 version of layer shell. Build a pinned
upstream release after installing the prerequisites above:

```sh
git clone --branch v1.0.4 --depth 1 \
  https://github.com/wmww/gtk4-layer-shell.git /tmp/gtk4-layer-shell
meson setup /tmp/gtk4-layer-shell/build /tmp/gtk4-layer-shell \
  --prefix=/usr \
  -Dexamples=false -Ddocs=false -Dtests=false -Dsmoke-tests=false \
  -Dintrospection=false -Dvapi=false
meson compile -C /tmp/gtk4-layer-shell/build
sudo meson install -C /tmp/gtk4-layer-shell/build
sudo ldconfig
```

Install `xwayland` separately if you want to test X11 applications.

### Vosk voice library

The `focaldesk-voice` crate links to the native `libvosk` shared library. Fedora
installations can provide it through the distribution's Vosk package. On
distributions without a packaged library, download the matching native archive
from the [official Vosk releases](https://github.com/alphacep/vosk-api/releases)
and make the directory containing `libvosk.so` available at build and runtime:

```sh
export LIBRARY_PATH=/path/to/vosk-library
export LD_LIBRARY_PATH=/path/to/vosk-library
```

A speech-recognition model is separate from this shared library. Configure its
location with `FOCALDESK_VOSK_MODEL_DIR`; do not commit downloaded models to the
repository.

## Clone and build

```sh
git clone https://github.com/sjweiler/focaldesk.git
cd focaldesk
cargo build --workspace
```

An optimized build takes longer but is more representative of compositor
performance:

```sh
cargo build --release --workspace
```

The workspace release profile enables thin link-time optimization and uses one
code-generation unit. This favors runtime performance and cross-crate
optimization at the cost of a slower release build; development builds keep
Cargo's normal faster iteration profile.

The repository also provides a `justfile`:

```sh
just build
```

## Run nested for development

Run the winit backend inside an existing Wayland session:

```sh
cargo run -p focaldesk-desktop --no-default-features --features winit,xwayland
```

An experimental wgpu compositor backend is also available. It deliberately
enables only wgpu's Vulkan backend, opens a nested window, advertises its own
Wayland socket, logs the selected adapter and driver, and presents frames while
handling resize and surface-loss events:

```sh
just nested-wgpu
```

It currently composites `ARGB8888` and `XRGB8888` Wayland SHM window trees,
including subsurfaces, popups, viewport crops/scaling, and layer-shell trees.
It also advertises single-plane `ARGB8888`/`XRGB8888` and
`ABGR8888`/`XBGR8888` DMA-BUFs. Supported tiled modifiers are queried from the
selected Vulkan physical device and published through linux-dmabuf feedback.
Those buffers are imported as Vulkan textures without a GPU copy; a synchronized
linear mapping/upload keeps the linear protocol path working on adapters without
external-memory support. When the Vulkan render node supports DRM syncobj
eventfds, `linux-drm-syncobj-v1` is exposed and acquire/release timeline points
are honored with asynchronous commit blockers and submission-bound buffer
retention. To try the SHM
path, find the `wayland_display` value in the initialization log and launch
`WAYLAND_DISPLAY=focaldesk-1 weston-simple-shm`. Host keyboard, pointer,
buttons, scrolling, focus, and cursor surfaces are wired through the normal
compositor input path. Buffer transforms and viewport texture coordinates are
supported. A first native shell pass draws themed topbar, sidebar, work-area,
status-well, and notification-card geometry around the client stack. The output
scene is retained between frames, so unchanged clients do not cause continuous
redraws and dirty output regions are recomposed through GPU scissors.
Wallpaper images honor the configured fit mode and tint/dim values; shell SVG
icons and cached font-atlas text are rendered with live UI-state colors. Rounded
panels are antialiased in the solid-quad shader. Multi-plane DMA-BUFs and full
shell effect parity are not wired yet. The
established winit/GLES backend remains the nested compositor used for full
compatibility testing.

Run the accelerated-client matrix with `just nested-wgpu-smoke`. It exercises
Weston's DMA-BUF demo plus GTK4 and Chromium/Chrome when installed, verifies a
direct non-linear Vulkan import when the adapter advertises one, and preserves
logs under `target/nested-wgpu-smoke`.

The desktop build contains both DRM renderers. `focaldmd` selects one for the
authenticated session with its top-level `renderer` setting:

```toml
# /etc/focaldmd.toml
renderer = "gles"    # recovery/default renderer
# renderer = "vulkan" # direct Vulkan DRM renderer
```

`just install-desktop` builds both paths into the same binary. The Vulkan path
uses raw ash: Smithay owns atomic KMS, GBM allocates scanout buffers, and Vulkan
imports those DMA-BUFs as color attachments. Each queue submission exports a
sync-file that Smithay passes to KMS as the primary plane input fence. It does
not use Vulkan display WSI and does not create EGL or GLES objects. The Vulkan
backend creates one scanout path per enabled connected output and honors saved
mode, scale, logical position, and primary-output settings. Use the GLES
renderer for HDR, output capture, XWayland, or live hotplug until those
lifecycle paths are implemented for Vulkan.

`cargo check -p focaldesk-desktop --no-default-features --features drm-vulkan,xwayland`
verifies the combined build without installing it. The legacy `drm-wgpu`
feature remains as a build-script compatibility alias.

This is the recommended development path because a compositor crash only closes
the nested window. Backend-specific DRM/KMS shortcuts such as screenshots are
not available in nested mode.

Increase diagnostic logging when needed:

```sh
RUST_LOG=debug cargo run -p focaldesk-desktop --no-default-features --features winit,xwayland
```

See [Troubleshooting](troubleshooting.md) for log locations and common failures.

Run the repeatable nested compatibility smoke test with:

```sh
just nested-smoke
```

It captures startup, registry, client, XWayland, and crash-check artifacts below
`target/nested-smoke`. See
[Compatibility Testing](compatibility-testing.md) for the matrix and options.

## Install a DRM/KMS Wayland session

The default compositor features build the direct DRM/KMS backend. Install the
release binary and session entry with:

```sh
just install-desktop
just install-desktop-session
```

These recipes install:

- `/usr/local/bin/focaldesk-desktop`
- `/usr/libexec/focaldesk/focaldesk-polkitd`
- `/usr/share/wayland-sessions/focaldesk.desktop`
- `/usr/lib/systemd/user/focaldesk-session.target`

FocalDesk leaves the platform sleep mode unchanged, including firmware-backed
`deep` sleep. The DRM backend retains the libseat-owned DRM device across
suspend, reconstructs scanout state after session activation, and forces a
complete post-resume modeset. NVIDIA renderers use implicit KMS synchronization
in every login so a suspend-induced driver failure cannot poison a later
session's initial modeset. The rail and dock are restarted after the first
recovered page flip to discard their independent GTK GPU caches.
`install-desktop` removes the legacy FocalDesk-owned NVIDIA `s2idle` override if
an older installation left it behind.

Log out, select **FocalDesk** in the display manager, and sign in. Keep a known
working session installed so you can recover from compositor or driver failures.

## Install desktop services

For a local, per-user service installation:

```sh
just install-services
```

This installs binaries under `~/.local/bin`, user units under
`~/.config/systemd/user`, reloads the user service manager, and enables the
core service set. The command-executing automation service is deliberately not
installed by this bundle; install it explicitly with
`just install-automation-service` only after reviewing its scripts and unit.
The experimental `focaldesk-remoted` binary and unit are installed but remain
disabled and stopped. See [Remote Desktop](remote-desktop.md) before starting it.
Fedora packagers can instead use:

```sh
just install-services-fedora
```

The Fedora recipes place binaries and unit files in system locations. Review
the recipes in `justfile` before using them in a packaging environment. Fedora
automation is likewise opt-in through `just install-automation-service-fedora`.

To install the AI backend and console on Fedora, use the dedicated recipe:

```sh
just install-ai-fedora
systemctl --user restart focaldesk-server.service
```

This installs `focaldesk-server` and `focaldesk-dialogd` under `/usr/bin` and
their user-session units under `/usr/lib/systemd/user`. They are intentionally
managed with `systemctl --user`, because the AI server needs the logged-in
desktop session's runtime directory and IPC sockets. The recipe also removes
older per-user development units from `~/.config/systemd/user`, which would
otherwise override the Fedora units.

Individual applications can be installed with recipes such as:

```sh
just install-launcher
just install-files
just install-settings
just install-ai-console
```

## Optional FocalDesk display manager

Fedora installations can install the native `focaldmd` daemon, greeter,
non-interactive greeter PAM policy, human-login PAM policy, configuration,
systemd unit, and `focaldm` system user definition with:

```sh
just install-focaldmd-fedora
```

The recipe intentionally does not enable or start the display manager. First
confirm that `/usr/local/bin/focaldesk-desktop` starts as a normal Wayland
session and keep the distribution's existing display manager available for
recovery. Switching the system `display-manager.service` alias is an
administrator action that should only be done after testing the installed
greeter and PAM policies on the target Fedora release.

## Development checks

Run the same baseline checks expected for pull requests:

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets
cargo test --workspace
```

Some hardware-specific behavior cannot be covered by unit tests. Mention the
GPU, driver, backend, output topology, and manual test performed when submitting
a compositor, HDR, capture, or multi-monitor change.

## Updating dependencies

Do not use `cargo update` as a general build-repair step: it changes
`Cargo.lock`. Update dependencies intentionally, review the lockfile diff, and
run the complete check suite. For ordinary build failures, first use the locked
versions already committed to the repository.

## Verify or uninstall an installation

Verify installed files and their expected permission modes:

```sh
just verify-install user
just verify-install system
```

The uninstaller removes only paths recorded in the reviewed manifests under
`packaging/`. It disables FocalDesk user services first and preserves settings,
secrets, installed themes, logs, and session state by default:

```sh
just uninstall user
just uninstall system
```

System removal asks for `sudo` only for system-owned files. To also delete all
FocalDesk user configuration and state, use `just uninstall-purge`; this is
irreversible and requires a separate confirmation. The generated
`~/.config/xdg-desktop-portal-wlr/config` is retained because the user may have
customized it or another compositor may use it.

## Next steps

- [Configuration](configuration.md)
- [Default keybindings](keybindings.md)
- [Troubleshooting](troubleshooting.md)
- [Architecture](architecture.md)
- [Roadmap](../ROADMAP.md)
