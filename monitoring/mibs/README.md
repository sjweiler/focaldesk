# FocalDesk Vulkan inventory MIB draft

`FOCAL-ROOT-MIB.txt` defines the enterprise branches and module identity
assignments. `FOCALDESK-MIB.txt` defines the read-only Vulkan inventory profile.
The [IANA PEN registry](https://www.iana.org/assignments/enterprise-numbers.txt) lists
67726 for Steven Weiler. FocalDesk objects are under `.1.3.6.1.4.1.67726.1.1`.

## Snapshot mapping

The input is the JSON from `scripts/focaldesk-monitor-inventory.py`. For each
request, use a single inventory snapshot. Scalar instance OIDs end in `.0`,
and column instances end in the persistent row's `index`.

| MIB objects | Inventory fields and conversion |
| --- | --- |
| `fdCompositorUptime` | `uptime_seconds * 100`, modulo 2^32 |
| `fdCompositorEpoch` | UTF-8 bytes of `producer_epoch` |
| `fdInventoryEpoch` | Decode the hex `inventory_epoch` into 16 bytes |
| `fdKeyboardSamples`, `fdKeyboardDelayUs` | Keyboard `samples` and `total_us` from `input_dispatch_delay.series` as Counter64 |
| `fdPointerSamples`, `fdPointerDelayUs` | Pointer motion `samples` and `total_us` from the same series as Counter64 |
| `fdGpuIndex`, `fdGpuIdentity`, `fdGpuState` | GPU `index`, UTF-8 `identity`, and `state` mapped to its named enum |
| `fdGpuDeviceLosses`, `fdGpuRebuildAttempts`, `fdGpuRebuildSuccesses` | `device_losses`, `rebuild_attempts`, `rebuild_successes` as Counter64 |
| `fdDisplayIndex`, `fdDisplayIdentity`, `fdDisplayScanoutGpuIndex` | Display `index`, UTF-8 `identity`, and `scanout_gpu_index` |
| `fdDisplayState` | Display `state` mapped to its named enum |
| `fdDisplayWidth`, `fdDisplayHeight`, `fdDisplayRefreshMilliHz` | `width`, `height`, and `refresh_millihz` as Gauge32 |
| `fdDisplayPresentedFrames` | `presented_frames` as Counter64 |
| `fdGpuBusyPercent`, `fdGpuVramUsedMiB`, `fdGpuVramTotalMiB` | Optional device-driver samples matched to the GPU by PCI address |
| GPU/display sample status and age | `sample_status` mapped to its named enum and `sample_age_seconds` as Gauge32 |

Null fields have no instance. A compositor epoch change invalidates counter
deltas. An inventory epoch change requires table rediscovery. The scanout GPU
comes from the connector's owner. The render-GPU column stays reserved pending
measurement of the actual render path. Render timing, service health, and notifications remain pending.

## Offline validation

Validate the production modules and translate a display counter instance.

```sh
mib_dir="$PWD/monitoring/mibs"
SMIPATH="$mib_dir:/usr/share/snmp/mibs" smilint -s -l 5 "$mib_dir/"FOCAL*.txt
snmptranslate -M "+$mib_dir" -m +FOCALDESK-MIB -On FOCALDESK-MIB::fdDisplayPresentedFrames.3
```

The translated instance is `.1.3.6.1.4.1.67726.1.1.3.1.10.3`.

## Read-only AgentX lab

The subagent serves the mapped objects on a Unix socket behind Net-SNMP. It
takes an explicit numeric `fdObjects` OID. The production value is
`.1.3.6.1.4.1.67726.1.1`. For a temporary lab, supply the
authorized numeric subtree directly. The documentation PEN is for offline
syntax checking only.

```sh
python3 monitoring/mibs/test-agentx-protocol.py
bash monitoring/mibs/test-agentx-lab.sh "$AUTHORIZED_FD_OBJECTS_OID"
```

The lab script requires Net-SNMP client tools, `snmpd`, and Python 3. It starts
a disposable master bound to `127.0.0.1:11661` and restricts SNMPv3 access
to a generated read-only authPriv user. The AgentX socket, credentials, engine
state, and temporary registry live in a private directory removed at exit.
Use a second argument to read a captured snapshot or set
`FOCALDESK_SNMP_LAB_PORT` if the default port is in use.

For a standalone lab process, run `scripts/focaldesk-monitor-agentx.py` with
`--socket`, `--root`, `--snapshot`, and `--registry`. Keep the socket private
to the master and subagent. The subagent reconnects after a master restart,
retains a bounded cache of per-request snapshots, and returns no rows when
its input cannot be read. Stale snapshots remain readable with stale status.
GPU recovery failure notifications are opt-in.

See the [OpenNMS collection profile](../opennms/README.md) for polling the lab.

### NVIDIA device sensors

Pass `--nvidia-smi /usr/bin/nvidia-smi` to the subagent to poll NVIDIA GPU busy
percentage and used/total VRAM in MiB. The subprocess runs in a background
worker and never blocks compositor rendering or the AgentX request handler.
The sampler runs approximately every five seconds and drops sensor values
older than 15 seconds. Unsupported values and stale compositor rows have
no sensor instance. The values describe the whole GPU, including applications
outside FocalDesk. The Intel GPU continues to expose its normal inventory and
recovery metrics.

### GPU recovery failure INFORM

The optional `fdGpuRecoveryFailed` notification is
`.1.3.6.1.4.1.67726.1.0.4`. It carries the indexed GPU identity, state,
rebuild attempts and successes, and compositor epoch. The adapter observes
fresh snapshots independently of SNMP polling. Its first snapshot in each
producer epoch establishes the baseline. An observed transition into
`failed` can emit one notification per GPU every 30 seconds.

Enable the adapter with `--gpu-failure-notification .1.3.6.1.4.1.67726.1.0.4`
after configuring the master with an SNMPv3 authPriv INFORM destination.
Use a private Net-SNMP `trapsess -Ci -v3` configuration with the receiver's
USM user, matching authentication and privacy settings, and bounded retries.
A successful AgentX response acknowledges the local handoff. The manager's
INFORM response acknowledges network delivery. Polling and thresholding
continue to cover missed notifications.

Test the full sender path against a disposable local receiver without
touching the compositor or OpenNMS:

```sh
bash monitoring/mibs/test-gpu-notification.sh
```

### Compositor freshness notifications

Enable `--compositor-notifications` to observe the compositor independently
of polling. After the first fresh snapshot, 30 seconds without a new sample
produces `fdCompositorMetricsStale` (`.1.3.6.1.4.1.67726.1.0.5`). This
covers an unreadable or missing snapshot as well as an old file. It reports
loss of telemetry rather than determining the process exit reason.

A new fresh sample after an interruption, or a new producer epoch, produces
`fdCompositorMetricsResumed` (`.1.3.6.1.4.1.67726.1.0.6`). The final
varbind indicates whether a new producer epoch was observed. A master
notification filter needs to allow both OIDs. The loopback test above also
checks these transitions.

### Display failure and recovery

Enable `--display-notifications` to send `fdDisplayFailed`
(`.1.3.6.1.4.1.67726.1.0.7`) and `fdDisplayRecovered`
(`.1.3.6.1.4.1.67726.1.0.8`). Each carries the indexed display identity,
state, failure and recovery counts, and compositor epoch. The first snapshot
for a display establishes the counter baseline. The counters preserve an
incident even when the output recovers between two samples. Unplug and disable
events do not increment either counter. Older compositor builds omit both
counters, so they cannot generate these notifications.

Re-import the updated OpenNMS event file, allow both OIDs in the master
notification filter, and enable the adapter flag after deploying the
updated compositor.
