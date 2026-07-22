# Troubleshooting

FocalDesk is alpha compositor and desktop-session software. Test from a nested
session first and keep another working login session available.

## Logs

The compositor tries log destinations in this order:

1. `FOCALDESK_LOG_FILE`, when set.
2. `$XDG_STATE_HOME/focaldesk/focaldesk.log`, when `XDG_STATE_HOME` is set.
3. `$XDG_CACHE_HOME/focaldesk/focaldesk.log`.
4. `~/.cache/focaldesk/focaldesk.log`.
5. `/tmp/focaldesk.log`.

On Linux, messages are also sent to the journal when journald is available.
Inspect a user service with:

```sh
systemctl --user status focaldesk-server.service
journalctl --user -u focaldesk-server.service -b
```

Inspect compositor messages for the current boot with:

```sh
journalctl -b -t focaldesk
```

Enable additional tracing for a nested run:

```sh
RUST_LOG=debug cargo run -p focaldesk-desktop --no-default-features --features winit,xwayland
```

Logs can contain filenames, application identifiers, model-provider errors, and
other private context. Review them before posting publicly.

## `cargo run` selects no binary

FocalDesk is a virtual Cargo workspace with many binaries. Name the compositor
package and backend explicitly:

```sh
cargo run -p focaldesk-desktop --no-default-features --features winit,xwayland
```

## Nested compositor does not start

- Confirm you are already inside a Wayland session: `echo "$WAYLAND_DISPLAY"`.
- Confirm the winit feature is selected and default DRM features are disabled.
- Run with `RUST_LOG=debug` and inspect the first error, not only the final
  shutdown message.
- Re-run `cargo check -p focaldesk-desktop --no-default-features --features winit,xwayland`.

## FocalDesk is absent from the display manager

Verify the installed files and paths:

```sh
test -x /usr/local/bin/focaldesk-desktop
test -f /usr/share/wayland-sessions/focaldesk.desktop
```

Re-run `just install-desktop` and `just install-desktop-session` after changing
the binary or session file, then log out completely.

## XWayland applications do not start

Verify that `Xwayland` is installed and visible:

```sh
command -v Xwayland
```

Capture compositor logs around XWayland startup. Include the distribution,
XWayland version, application, and whether the failure occurs in nested or
DRM/KMS mode when reporting the issue.

## A background feature is unavailable

Check the session target and failed user services:

```sh
systemctl --user status focaldesk-session.target
systemctl --user --failed
```

After changing a unit or drop-in:

```sh
systemctl --user daemon-reload
systemctl --user restart SERVICE_NAME.service
```

Do not run user services with `sudo`; that connects to the wrong service
manager and runtime directory.

## PipeWire capture fails

Confirm PipeWire and the desktop portal are running, then inspect their user
journals. Capture is experimental and currently depends on the portal setup
installed by `just install-portal`. If output selection fails, list the connector
names from FocalDesk's display settings and test `FOCALDESK_SCREENCAST_OUTPUT`
with the intended connector name.

## DRM/KMS session fails or leaves a blank screen

Switch to another virtual terminal using the distribution's normal
`Ctrl+Alt+F<n>` shortcut, sign in, and inspect the current boot journal. Record:

- GPU model and driver version.
- Kernel and distribution version.
- Connector names, modes, refresh rates, scales, and transforms.
- Whether HDR, multiple GPUs, or multiple outputs were enabled.
- The last compositor log messages before failure.

Do not enable undocumented HDR or atomic-commit overrides as a general fix.

## Build failures after an update

Use the committed lockfile first:

```sh
cargo check --workspace
```

Avoid `cargo update` and `cargo clean` as first-line fixes. `cargo update`
changes dependency resolution, while `cargo clean` discards useful incremental
artifacts without addressing missing packages or compiler errors.

## Reporting a bug

Include expected behavior, actual behavior, minimal reproduction steps, backend,
distribution, kernel, GPU and driver, output topology, relevant logs, and the
commit tested. Remove credentials and private data. Report vulnerabilities
through the private process in [SECURITY.md](../SECURITY.md), not a public issue.
