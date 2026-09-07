# Remote Desktop

FocalDesk has an experimental Phase 3 remote-desktop path for viewing one
physical output through RDP. It is deliberately narrow: one view-only client,
one selected output, full-frame bitmap updates, and a loopback-only listener.
Input, clipboard synchronization, direct LAN exposure, independent login
sessions, and production authentication are not implemented.

## Security model

`focaldesk-remoted` terminates RDP over TLS outside the compositor process. It
receives frames through a private Unix socket using versioned, size-bounded JSON
messages and sealed `memfd` file descriptors. The compositor checks the peer UID
and executable or systemd-unit identity before allowing capture.

The IronRDP dependency graph also contains an RSA implementation for its
Hybrid/CredSSP mode. FocalDesk does not enable that mode: it selects TLS security
with a rustls acceptor and never performs private-key operations through the
unused RSA dependency. This boundary is documented beside the temporary RustSec
exception in CI and must be revisited when IronRDP makes CredSSP optional.

Phase 3 adds these additional constraints:

- The daemon does nothing unless started with `--enable`.
- Only loopback bind addresses are accepted; the default is `127.0.0.1:3389`.
- Remote input and clipboard channels are disabled.
- A random 48-character token expires 15 minutes after service startup.
- The token, private key, and certificate use owner-only runtime files.
- The self-signed TLS certificate is regenerated each time the service starts.
- Only one RDP connection is processed at a time. Later connections wait in the
  listener backlog rather than receiving concurrent desktop access.

The Phase 3 identity is suitable for development, not unattended or
internet-facing access. Keep the listener on loopback and use an authenticated
SSH tunnel from another machine. Production certificate identity, authentication
rate limiting, local confirmation, and compositor-owned connection indicators
remain later roadmap work.

The compositor begins capture when `focaldesk-remoted` starts, before an RDP
client authenticates, and continues while the daemon is running. Stop the service
when testing is complete.

## Install and start

For a local per-user installation:

```sh
just install-remoted-service
systemctl --user start focaldesk-remoted.service
cat "$XDG_RUNTIME_DIR/focaldesk/remoted-token.json"
```

The install recipe reloads the user service manager but intentionally does not
enable or start the unit. The credentials file contains the username
`focaldesk`, the token, and its Unix expiration time. Connect a local RDP client
to `127.0.0.1:3389`.

The complete local service bundle and the Fedora service bundle also install the
remote-desktop unit without enabling it:

```sh
just install-services
# Fedora system paths:
just install-services-fedora
```

## Connect through SSH

Run the tunnel on the computer containing the RDP client:

```sh
ssh -N -L 13389:127.0.0.1:3389 user@focaldesk-host
```

Keep that SSH process running and point the RDP client at `127.0.0.1:13389`.
The RDP stream remains TLS-encrypted inside the authenticated SSH tunnel. Do not
forward port 3389 from a router or modify the service to listen on all interfaces.

## Select an output

The packaged development unit requests output ID 1. To test another output,
stop the unit and run the installed daemon directly:

```sh
systemctl --user stop focaldesk-remoted.service
~/.local/bin/focaldesk-remoted --enable --output 2
```

The daemon negotiates the selected output dimensions and resizes the RDP desktop
if the compositor reports a geometry change.

## Stop and troubleshoot

Stop capture and remove the listener with:

```sh
systemctl --user stop focaldesk-remoted.service
```

Inspect service logs with:

```sh
journalctl --user -u focaldesk-remoted.service -b
```

If a new connection is rejected after 15 minutes, restart the service and read
the newly generated token. If startup reports that the capture socket is absent,
confirm the FocalDesk compositor is running and that
`$XDG_RUNTIME_DIR/focaldesk/remote-capture.sock` exists.

Live interoperability with two independent client implementations remains an
explicit Phase 3 validation item. See the
[remote-desktop roadmap](remote-desktop-roadmap.md) for the remaining phases and
release criteria.
