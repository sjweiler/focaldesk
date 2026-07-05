

# Building FocalDesk

This guide explains how to build and run FocalDesk from source.

## Supported Platforms

FocalDesk is currently developed and tested on:

- Fedora Workstation
- Wayland
- Rust stable

Other distributions may work but are not officially tested.

---

# Prerequisites

## Rust

Install Rust using rustup:

```bash
curl https://sh.rustup.rs -sSf | sh
```

Verify:

```bash
cargo --version
rustc --version
```

---

## System Packages

### Fedora

Install the required development packages:

```bash
sudo dnf install \
    meson \
    ninja-build \
    cmake \
    gcc \
    clang \
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
    gtk4-devel \
    libadwaita-devel \
    xorg-x11-server-Xwayland
```

Additional packages may be required as development continues.

---

# Clone the Repository

```bash
git clone https://github.com/sjweiler/focaldesk.git
cd focaldesk
```

---

# Build

Compile the workspace:

```bash
cargo build
```

For an optimized release build:

```bash
cargo build --release
```

---

# Running

### Nested (Development)

Run inside an existing Wayland session:

```bash
cargo run
```

or

```bash
cargo run --release
```

This is the recommended development workflow.

---

### DRM/KMS Session

Running directly on DRM/KMS requires appropriate permissions and a supported Linux system.

This mode is intended for testing a complete desktop session.

---

# Workspace Layout

The project is organized as a Rust workspace.

Major crates include:

- FocalDesk compositor
- Launcher service
- Shared IPC library
- Settings application
- File manager

Additional crates may be added over time.

---

# Optional Features

Depending on your hardware and installed packages, FocalDesk may support:

- XWayland
- PipeWire capture
- Hardware cursor
- Multi-monitor
- HDR experimentation

---

# Debug Builds

For development:

```bash
cargo build
```

Debug builds include symbols and are recommended while developing.

---

# Release Builds

```bash
cargo build --release
```

Release builds provide significantly better rendering performance.

---

# Logging

Increase logging output using:

```bash
RUST_LOG=debug cargo run
```

or

```bash
RUST_LOG=trace cargo run
```

If using the built-in logging system, refer to the logging documentation.

---

# Common Problems

## Missing system libraries

Ensure all required development packages are installed.

---

## XWayland not launching

Verify that XWayland is installed:

```bash
which Xwayland
```

---

## Build fails after dependency updates

Update dependencies:

```bash
cargo update
```

Clean the build if necessary:

```bash
cargo clean
cargo build
```

---

## Permission issues

DRM/KMS mode typically requires:

- seatd
- logind
- appropriate user permissions

Nested mode does not require these privileges.

---

# Development Workflow

Typical workflow:

```bash
git pull

cargo fmt

cargo clippy

cargo test

cargo run
```

Before submitting changes:

```bash
cargo fmt
cargo clippy --all-targets
cargo test
```

---

# Continuous Integration

GitHub Actions automatically builds and validates the project on every push and pull request.

Checks include:

- formatting
- compilation
- linting
- tests

---

# Next Steps

After successfully building FocalDesk, see:

- `architecture.md`
- `vision.md`
- `roadmap.md`
