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

If AI chat reports that it is blocked for a provider, inspect the permission
mode and the dialog broker:

```sh
systemctl --user --no-pager show focaldesk-server.service -p Environment
systemctl --user status focaldesk-dialogd.service
```

For Fedora installations, reinstall the matching system-installed user units
with `just install-ai-fedora`; do not mix them with the local development
recipe `just install-ai`. Fedora units live under `/usr/lib/systemd/user` but
still use `systemctl --user` because they run inside the graphical session.
After changing a unit, reload and restart it:

```sh
systemctl --user daemon-reload
systemctl --user restart focaldesk-server.service
```

Inspect compositor messages for the current boot with:

```sh
journalctl -b -t focaldesk
```

Enable additional tracing for a nested run:

```sh
RUST_LOG=debug cargo run -p focaldesk-desktop --no-default-features --features winit,xwayland
```

For a bounded reproduction with collected artifacts, run `just nested-smoke`.
Attach the relevant files from `target/nested-smoke` after reviewing them for
private information. See [Compatibility Testing](compatibility-testing.md).

Logs can contain filenames, application identifiers, model-provider errors, and
other private context. Review them before posting publicly.

## Diagnostic bundles and crash reports

Create a bounded diagnostic archive for a bug report with:

```sh
focaldesk-cli diagnostics
```

The command prints the archive path and includes system/session metadata, DRM
connector and GPU context, user-service state, recent FocalDesk journal entries,
the active FocalDesk log, and the latest crash report when available. Choose a
specific destination or omit logs when only system context is needed:

```sh
focaldesk-cli diagnostics --output focaldesk-report.tar.gz
focaldesk-cli diagnostics --no-logs
```

Bundle entries and the archive use owner-only permissions. Collection is
bounded, and common credential shapes, home paths, usernames, and hostnames are
redacted automatically. Redaction is best effort: inspect the archive before
sharing it.

Processes using `focaldesk-logging` save the latest panic and backtrace under
`$XDG_STATE_HOME/focaldesk/crashes/latest.txt`, or the corresponding
`~/.local/state` location when `XDG_STATE_HOME` is unset. This file is also
owner-only and is replaced atomically.

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

### Blank display after suspend

First compare FocalDesk with another Wayland compositor on the same installed
kernel, GPU driver, firmware, and display topology. If the other compositor
resumes correctly, investigate FocalDesk's display-pipeline reconstruction
before changing the kernel sleep mode.

The DRM backend records monotonic `t_ms` values while moving through:

```text
Running -> Suspending -> SessionInactive -> Resuming -> Reprobing -> Modesetting -> Running
```

The journal should contain `prepare-for-sleep(true)`, rendering stopped and the
number of abandoned flips, `prepare-for-sleep(false)`, DRM device activation,
connector reprobe, the first post-resume atomic modeset being queued, and its
page-flip completion. `render_frame` and `queue_frame` failures include their
full debug error chain so a DRM errno is retained when the backend exposes one.

After activation, FocalDesk resets connector and plane state through the
existing libseat-owned DRM file descriptor. It then invalidates compositor GPU
caches, requests full output damage, and uses the next frame as a complete
modeset rather than reusing the pre-suspend page-flip state. Connector changes
received while the session is inactive are deferred until ownership returns.

For isolation, begin with one display at 60 Hz using SDR and 8-bit scanout, with
the hardware cursor disabled. Re-enable native resolution, the hardware cursor,
a second display, high refresh, 10-bit scanout, and HDR in that order. If minimal
SDR resumes but HDR does not, capture the first post-resume commit failure and
the connector HDR-property readback. If minimal SDR also fails, focus on the
libseat activation, connector reprobe, and first modeset events.

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
