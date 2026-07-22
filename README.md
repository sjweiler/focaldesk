# FocalDesk

[![CI](https://github.com/sjweiler/focaldesk/actions/workflows/ci.yml/badge.svg)](https://github.com/sjweiler/focaldesk/actions/workflows/ci.yml)
[![CodeQL](https://github.com/sjweiler/focaldesk/actions/workflows/codeql.yml/badge.svg)](https://github.com/sjweiler/focaldesk/actions/workflows/codeql.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/Rust-stable-orange)
![Wayland](https://img.shields.io/badge/Wayland-native-blue)
![Status](https://img.shields.io/badge/status-alpha-red)

FocalDesk is an experimental Wayland desktop environment built around a custom
Rust compositor and a cohesive system-experience layer. It is alpha software:
use it for development and testing, not as the only session protecting important
work.

The long-term direction is a fast, keyboard-friendly, retro-futuristic desktop
with structured workspaces, clear system feedback, and permissioned automation.
See the [project vision](docs/vision.md), [architecture](docs/architecture.md),
and [roadmap](ROADMAP.md) for the intended direction.

## Project Status

FocalDesk is developed and used by its project owner, but APIs, configuration,
and behavior may change without compatibility guarantees before a stable
release.

| Area | Status |
| --- | --- |
| Wayland compositor and desktop shell | Working, alpha |
| DRM/KMS and nested winit backends | Working, alpha |
| Workspaces and multi-monitor layout | Working, with ongoing edge-case work |
| XWayland application support | Working, alpha |
| PipeWire/portal screen capture | Experimental |
| HDR and color management | Experimental and hardware-dependent |
| Settings, file manager, launcher, and AI console | Usable prototypes |
| Local AI and automation services | Experimental and permission-gated |
| Precise Wayland subsurface damage tracking | Planned |

The table describes repository capabilities, not a compatibility guarantee for
every distribution, GPU, or application. See [Building FocalDesk](docs/building.md)
and [Troubleshooting](docs/troubleshooting.md) before testing it.

## Screenshots

### Desktop

The main desktop screenshot shows the compositor, shell chrome, workspace UI,
and native applications running together. The repository contains:

- Native Wayland compositor
- Desktop shell
- Display manager (focaldmd)
- Login greeter
- Session manager
- System services and IPC framework
- First-party desktop applications
- Integrated local AI framework

![FocalDesk Desktop](docs/screenshots/desktop.png)

### Launcher

The native launcher provides application discovery and starts applications
through a dedicated launcher service rather than embedding process management
in the compositor.

![Launcher](docs/screenshots/launcher.png)

### Settings

The settings prototype exposes appearance, display, input, privacy, power, and
service controls. Some panels are still incomplete or hardware-dependent.

![Settings](docs/screenshots/settings.png)

### File Manager

The first-party file manager prototype provides common browsing, navigation,
search, sorting, and list/grid workflows.

![File Manager](docs/screenshots/files.png)

### AI Console permission prompt

This view demonstrates an AI request stopping at the runtime permission boundary
before an action proceeds.

![AI Console displaying a permission request](docs/screenshots/ai-console-permission-check.png)

### AI Console processing a query

The AI Console prototype connects to configured model providers through local
services. Provider availability and data handling depend on the user's selected
backend.

![AI Console processing a model query](docs/screenshots/ai-console-processing-query.png)

### Wine/DXVK rendering

This compatibility test shows World of Warcraft running through Wine/DXVK and
XWayland. It demonstrates a known-working setup, not compatibility with every
game, Wine version, or GPU driver.

![World of Warcraft running through Wine and DXVK](docs/screenshots/wow.png)

### OBS (Open Broadcaster Software)

OBS Studio is shown capturing a FocalDesk output through PipeWire and the
Wayland portal path. Screen capture remains experimental.

![OBS Studio capturing a FocalDesk output through PipeWire](docs/screenshots/obs.png)

## Repository Layout

- `apps/focaldesk-desktop`: compositor executable
- `apps/focaldesk-cli`: command-line interface
- `apps/focaldesk-ai-console`: interface app for ai
- `apps/focaldesk-files`: file app prototype
- `apps/focaldesk-settings`: settings app prototype
- `apps/focaldesk-portal`: portal-related app code
- `services/`: background daemons and IPC services
- `crates/`: shared FocalDesk libraries
- `assets/`: bundled visual assets
- `docs/`: design notes and architecture material

## Requirements

FocalDesk is currently developed as a Rust workspace for Linux.

Install Rust from <https://rustup.rs/> and follow the Fedora or Ubuntu dependency
instructions in [docs/building.md](docs/building.md). Fedora is the primary
development environment; Ubuntu is continuously compile-checked in CI.

## Build

```sh
cargo build --workspace
```

Or, if you have `just` installed:

```sh
just build
```

## Check

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

## Run

To run the compositor nested inside an existing Wayland session:

```sh
cargo run -p focaldesk-desktop --no-default-features --features winit
```

The default `focaldesk-desktop` features target a direct DRM/KMS session. Do not
run that mode casually from an important graphical session; use the installed
Wayland session described below.

To make FocalDesk show up in GDM, install the compositor and Wayland session:

```sh
just install-desktop
just install-desktop-session
```

That recipe builds a release binary and installs it to
`/usr/local/bin/focaldesk-desktop` (same path as the Wayland session `Exec=`).
Re-run `just install-desktop-session` after changing the session file.

To build and install the file manager prototype:

```sh
just install-files
```

That recipe builds a release binary and installs it to
`/usr/local/bin/focaldesk-files`.

To build and install the settings app:

```sh
just install-settings
```

That recipe builds a release binary and installs it to
`/usr/local/bin/focaldesk-settings`.

Because FocalDesk is alpha compositor/system software, run it from a safe
development session first. Avoid switching important work over to it until you
know the current state of your local build.

## AI Service

The background server exposes AI chat over the local IPC socket used by
`focaldesk-cli ai ...`.

The socket path resolves from `FOCALDESK_AI_SOCKET` first, then
`$XDG_RUNTIME_DIR/focaldesk-ai.sock` inside a user session, and finally
`/tmp/focaldesk-ai.sock` when no runtime directory is available.

By default the AI service asks the compositor to show a native approval modal,
logs each request, and records the decision through the normal FocalDesk
logging pipeline. If the desktop socket is unavailable, it falls back to the
service terminal.

You can tighten or relax the permission gate with:

- `FOCALDESK_AI_PERMISSION=prompt` to ask via the compositor modal before each request
- `FOCALDESK_AI_PERMISSION=allow-session` to allow chat for the current session
- `FOCALDESK_AI_PERMISSION=allow-persistent` to persist the allow decision on disk across restarts
- `FOCALDESK_AI_PERMISSION=deny` to block AI chat

## Systemd Services

FocalDesk uses `systemd --user` for its background daemons. That is the
supported install path for the current codebase.

For a local build, install and enable the full service set with:

```sh
just install-services
```

That installs and enables:

- `focaldesk-server`
- `focal-launchd`
- `focaldesk-powerd`
- `focaldesk-notificationsd`
- `focaldesk-dialogd`
- `focaldesk-controlsd`
- `focaldesk-settingsd`
- `focaldesk-automation`
- `focald-voice`
- `focald-speech`
- `focald-mic`

Each unit lives under `packaging/systemd/user/` and is copied to
`~/.config/systemd/user/` for a local install.

If you are packaging for Fedora, use:

```sh
just install-services-fedora
```

That uses the Fedora unit variants under `packaging/systemd/user/*-fedora.service`
and installs the binaries into `/usr/bin/`.

If you only want one service, the per-daemon `just install-*-service` recipes
still work.

### Text-to-speech backends

`focald-speech` uses eSpeak NG by default. It can instead synthesize with Piper
and send Piper's raw PCM stream directly to PipeWire. Create a user-service
drop-in directly:

```sh
mkdir -p ~/.config/systemd/user/focald-speech.service.d
$EDITOR ~/.config/systemd/user/focald-speech.service.d/override.conf
```

Add the following override with the path to a downloaded `.onnx` voice model
(the matching `.onnx.json` file must be alongside it):

```ini
[Service]
Environment=FOCALD_SPEECH_BACKEND=piper
Environment=FOCALD_SPEECH_PIPER_MODEL=/path/to/en_US-lessac-medium.onnx
# Set this when Piper is installed outside systemd's PATH.
Environment=FOCALD_SPEECH_PROGRAM=/absolute/path/to/piper
# Match the sample rate in the model's adjacent JSON configuration.
Environment=FOCALD_SPEECH_PIPER_SAMPLE_RATE=22050
```

Then run `systemctl --user daemon-reload` and restart the daemon with
`systemctl --user restart focald-speech.service`. Piper and `pw-play` must be
installed. `FOCALD_SPEECH_PLAYER` can override the player executable. Set
`FOCALD_SPEECH_BACKEND=espeak-ng` (or remove the override) to return to eSpeak
NG.

## Documentation

- [Building and installation](docs/building.md)
- [Configuration and environment](docs/configuration.md)
- [Default keybindings](docs/keybindings.md)
- [Troubleshooting and logs](docs/troubleshooting.md)
- [Architecture](docs/architecture.md)
- [IPC design](docs/ipc.md)
- [Project roadmap](ROADMAP.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)

## License

FocalDesk is licensed under the MIT License. See [LICENSE](LICENSE).

Bundled third-party assets retain their own licenses, including the icon and font license files under `assets/`.
