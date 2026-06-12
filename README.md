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

Because FocalDesk is alpha compositor/system software, run it from a safe
development session first. Avoid switching important work over to it until you
know the current state of your local build.

## License

FocalDesk is licensed under the MIT License. See [LICENSE](LICENSE).

Bundled third-party assets retain their own licenses, including the icon and font license files under `assets/`.
