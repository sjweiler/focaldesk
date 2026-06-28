# FocalDesk

FocalDesk is an alpha desktop environment project built around a custom Wayland compositor and a cohesive system experience layer.

The long-term direction is a fast, keyboard-friendly, retro-futuristic desktop with structured workspaces, clear system feedback, and permissioned automation. See [docs/vision.md](docs/vision.md) for the broader project direction.

## Status

FocalDesk is alpha software.

It is stable enough for daily use by the project owner, but it is still early in
development. Expect incomplete features, breaking changes, rough edges, and
occasional compositor-level bugs. If you try it as a daily driver, be prepared
to debug Linux desktop, Wayland, graphics, and input issues.

## Repository Layout

- `apps/focaldesk-desktop`: compositor executable
- `apps/focaldesk-cli`: command-line interface
- `apps/focaldesk-ai-console`: interface app for ai
- `apps/focaldesk-files`: file app prototype
- `apps/focaldesk-settings`: settings app prototype
- `apps/focaldesk-portal`: portal-related app code
- `services/focaldesk-server`: background server and IPC daemon
- `crates/`: shared FocalDesk libraries
- `assets/`: bundled visual assets
- `docs/`: design notes and architecture material

## Requirements

FocalDesk is currently developed as a Rust workspace for Linux.

Install Rust from <https://rustup.rs/>.

On Debian/Ubuntu-style systems, the CI environment installs:

```sh
sudo apt install -y \
  pkg-config \
  libwayland-dev \
  libxkbcommon-dev \
  libudev-dev \
  libinput-dev \
  libegl1-mesa-dev \
  libgles2-mesa-dev \
  libgbm-dev \
  libgtk-4-dev \
  libadwaita-1-dev
```

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
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Run

The main compositor binary is:

```sh
cargo run -p focaldesk-desktop
```

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

## Systemd Service

The repo includes a user service unit at
[packaging/systemd/user/focaldesk-server.service](packaging/systemd/user/focaldesk-server.service).

To build and install it with `just`:

```sh
just install-server-service
```

That recipe builds a release binary, installs it to `~/.local/bin`, copies the
unit to `~/.config/systemd/user`, and enables the service.

If you are packaging for Fedora, install the binary to `/usr/bin/focaldesk-server`
and place the user unit in `/usr/lib/systemd/user/focaldesk-server.service`
using [packaging/systemd/user/focaldesk-server-fedora.service](packaging/systemd/user/focaldesk-server-fedora.service).

Use the Fedora recipe if you want the standard distro locations:

```sh
just install-server-service-fedora
```

## License

FocalDesk is licensed under the MIT License. See [LICENSE](LICENSE).

Bundled third-party assets retain their own licenses, including the icon and font license files under `assets/`.
