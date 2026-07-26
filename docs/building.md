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
  libadwaita-devel \
  xorg-x11-server-Xwayland
```

### Ubuntu and Debian-derived distributions

The CI build uses:

```sh
sudo apt update
sudo apt install -y \
  pkg-config \
  libwayland-dev \
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

The repository also provides a `justfile`:

```sh
just build
```

## Run nested for development

Run the winit backend inside an existing Wayland session:

```sh
cargo run -p focaldesk-desktop --no-default-features --features winit,xwayland
```

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
Fedora packagers can instead use:

```sh
just install-services-fedora
```

The Fedora recipes place binaries and units in system locations. Review the
recipes in `justfile` before using them in a packaging environment. Fedora
automation is likewise opt-in through `just install-automation-service-fedora`.

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

## Uninstall a local development installation

FocalDesk does not yet provide a packaged uninstaller. Disable user services
before removing files:

```sh
systemctl --user disable --now focaldesk-session.target
systemctl --user daemon-reload
```

Then remove only the FocalDesk files installed by the relevant `just` recipes.
The system session files are listed above, and per-user files are placed under
`~/.local/bin` and `~/.config/systemd/user`. Preserve
`~/.config/focaldesk` if you want to keep settings and AI permission records.

## Next steps

- [Configuration](configuration.md)
- [Default keybindings](keybindings.md)
- [Troubleshooting](troubleshooting.md)
- [Architecture](architecture.md)
- [Roadmap](../ROADMAP.md)
