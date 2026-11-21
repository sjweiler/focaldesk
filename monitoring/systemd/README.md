# Install the FocalDesk SNMP user service

This service runs Net-SNMP and the FocalDesk AgentX adapter in the rootless
Podman network namespace. It expects a metrics-enabled FocalDesk session
writing `$XDG_RUNTIME_DIR/focaldesk-vulkan-metrics.json`. Set
`FOCALDESK_METRICS_PATH` at compositor launch as described in the
[observability guide](../../docs/observability.md#local-vulkan-gpu-and-display-snapshots).

From the repository root, install the launcher and user unit:

```sh
state="${XDG_STATE_HOME:-$HOME/.local/state}/focaldesk/snmp-67726"
units="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
install -d -m 700 "$state" "$state/state"
install -d "$units"
install -m 700 monitoring/systemd/run-focaldesk-snmp.sh "$state/run.sh"
install -m 600 scripts/focaldesk-monitor-{agentx,inventory}.py "$state/"
install -m 644 packaging/systemd/user/focaldesk-snmp-67726.service "$units/"
```

For a new instance, copy [master.conf.example](master.conf.example) to
`$state/master.conf` with mode 0600. Set `agentAddress` to the private
OpenNMS Podman bridge address and UDP port. Keep the standard system view
and FocalDesk branch in the read-only view. The launcher puts the private
AgentX socket in the service runtime directory.

In a **new** state directory, create `$state/state/snmpd.conf` with mode
0600 and a single line in this format:

```text
createUser focalLab SHA-256 AUTH_PASSPHRASE AES PRIVACY_PASSPHRASE
```

Replace the placeholders with generated secrets before startup and use the same
credentials in the OpenNMS SNMPv3 polling definition. Net-SNMP replaces the seed
with localized credentials and boot state. When restoring an existing service,
reuse its persistent `state/snmpd.conf` and `indexes.sqlite3` instead of reseeding.
This preserves its SNMP engine ID and inventory epoch. Back up those files with
the service stopped and store the backup privately.

To enable INFORMs, configure the OpenNMS receiver as described in the
[OpenNMS setup](../opennms/README.md#gpu-recovery-failure-event) and create
`$state/notify-client.conf` with mode 0600:

```text
defSecurityName RECEIVER_USER
defAuthPassphrase RECEIVER_AUTH_PASSPHRASE
defPrivPassphrase RECEIVER_PRIVACY_PASSPHRASE
```

The launcher uses SHA-256, AES and authPriv, and accepts letters, digits,
dots, underscores, and hyphens in these three values. Defaults are
`opennms-horizon` on `opennms-net`, port `1162`. Override them via a systemd
user drop-in using `FOCALDESK_OPENNMS_CONTAINER`, `FOCALDESK_OPENNMS_NETWORK`,
or `FOCALDESK_OPENNMS_TRAP_PORT`. Omitting the notification client file
allows polling by itself. The service discovers the receiver address at
startup and restarts if it changes.

```sh
systemctl --user daemon-reload
systemctl --user enable --now focaldesk-snmp-67726.service
systemctl --user status focaldesk-snmp-67726.service
```

For startup before login, enable user-manager lingering through systemd if your
account does not already have it. The compositor must be running before fresh
metrics are available.

## FocalVector polling

Use a `focal-server` build that supports `FOCAL_SNMP_SNAPSHOT_SOCKET`. The
packaged FocalVector units set it to `%t/focaldesk/focal-vector-stats.sock`.
Update the installed FocalVector unit and SNMP launcher and unit, then install
the adapter from the FocalVector checkout into the same state directory:

```sh
install -m 600 ../focal-vector/monitoring/focal-vector-agentx.py "$state/"
```

For an existing SNMP deployment, add the following line to
`$state/master.conf` using the view assigned to your SNMPv3 polling user:

```text
view focaldeskView included .1.3.6.1.4.1.67726.2.1
```

Reload the systemd user units and restart the active FocalVector unit and
`focaldesk-snmp-67726.service`. Load `FOCALVECTOR-MIB.txt` from the FocalVector
checkout into the manager alongside `FOCAL-ROOT-MIB.txt`. Using the existing
SNMPv3 authPriv profile, walk `.1.3.6.1.4.1.67726.2.1`. It exposes daemon
uptime and restart epoch, collection statistics, and search backend status.
The adapter persists collection indexes in `$state/vector-indexes.sqlite3`.
