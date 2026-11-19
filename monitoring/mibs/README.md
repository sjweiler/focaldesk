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
Notifications are pending.

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
