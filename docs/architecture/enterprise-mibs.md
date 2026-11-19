# Enterprise monitoring and MIB design

Status: proposed, 2026-11-18. The local Vulkan snapshot and persistent
inventory reader are implemented. See [observability](../observability.md#persistent-monitoring-inventory).
The [read-only FocalDesk MIB draft](../../monitoring/mibs/README.md) covers
the available inventory fields. A read-only AgentX adapter and disposable SNMPv3 lab runner are available.
IANA assigned PEN 67726 to Steven Weiler. The production root and FocalDesk
inventory MIB are available.

## Namespace and modules

Use one enterprise root, `focalEnterprise = enterprises.67726`, with
these proposed child assignments:

| Child | Symbol | Module / purpose |
| --- | --- | --- |
| 1 | focalDesk | FOCALDESK-MIB: compositor, GPUs, displays, Wayland |
| 2 | focalVector | FOCALVECTOR-MIB: databases, queries, indexing, CUDA |
| 3 | focalPBX | Reserved for future telephony monitoring |
| 4 | focalAI | Reserved for inference and model management |
| 5 | focalCommon | FOCAL-COMMON-MIB: service health and shared conventions |
| 6 | focalModules | Module identities, allocated separately from object trees |

FOCAL-ROOT-MIB will define the root, branches, and module-identity registry.
FOCAL-TC-MIB will define common textual conventions. Each product and common
branch reserves `.0` for notifications, `.1` for objects, and `.2` for conformance
(`.2.1` groups, `.2.2` compliance). Allocate module identities consecutively under
`focalModules`: root=1, TC=2, common=3, desk=4, vector=5.

The production root is assigned in the
[IANA PEN registry](https://www.iana.org/assignments/enterprise-numbers.txt).
Keep private lab OIDs and their collected history separate from the production
namespace. The product-relative assignments are shared.

One PEN with internal subassignments is supported by
[RFC 9371 section 2.1](https://www.rfc-editor.org/rfc/rfc9371.html#section-2.1).
That section also specifies publication of the registered contact name and email.

## Shared collection architecture

Producers (compositor, vector daemon, service supervisor) publish typed snapshots
and bounded event records through local IPC to an unprivileged monitoring sidecar.
The proposed shared Rust metrics model owns identities, units, counters, windows,
and freshness; SNMP, OTLP/Prometheus, and dashboard adapters consume that model.
Do not use a sampled trace stream or scrape log messages as the source of truth.
Do not perform SNMP/network work, blocking IPC, or vendor queries on render threads.
Use bounded queues; expose dropped-event counts and refresh snapshots independently.

Existing `focaldesk-logging/src/metrics.rs` supplies AI operation counters and
histograms, not a hardware inventory or SNMP model. Preserve its fixed outcome
semantics and seconds-based OTLP histograms. Add the shared model alongside this
integration rather than introducing a competing query count. Instrument vector
queries at the daemon boundary; client and daemon observations are distinct and
must not be summed. FocalVector details below are proposed contracts requiring
validation in its own repository, not claims about implemented instrumentation.

Prefer a Rust AgentX subagent behind a maintained Net-SNMP SNMPv3 master over
implementing USM, VACM, and protocol cryptography in the compositor. Confirm a
maintained Rust AgentX implementation supports Counter64, sparse tables,
notifications, and reconnect before selecting it. Restrict AgentX to a protected
local socket. Configure the master for read-only `authPriv`, a restricted view of
the product/common branches, explicit manager access, persistent engine identity
and engine-boots state, and credentials outside repository/config examples.
Reject SET operations and disable v1/v2c in the deployment profile. Remote access
is opt-in. The first deliverable is a loopback-only lab agent, not a running change
to this workstation. See [USM](https://www.rfc-editor.org/rfc/rfc3414.html) and
[VACM](https://www.rfc-editor.org/rfc/rfc3415.html).

## Value and identity contracts

- Use Counter64 for cumulative events; rates are manager-side counter deltas.
  Counters survive polls, not necessarily producer restarts. A device-context
  rebuild must not reset counters held by the surviving compositor process.
- Use Gauge32 for bounded instantaneous counts and fixed-point measurements.
  Use CounterBasedGauge64 from HCNUM-TC for current byte sizes and vector counts
  that can decrease and exceed 32 bits; plain Counter64 is not a memory gauge.
- TimeTicks is hundredths of seconds, wrapping at 2^32. Uptime means producer
  process uptime, separately from the SNMP master's sysUpTime. Discontinuity
  timestamps use the master's sysUpTime clock; producer epochs identify restarts
  and resolve long-lived/wrapped-clock ambiguity.
- Every table has a supported-capabilities bitmap, sample status
  `ok(1), unsupported(2), unavailable(3), stale(4)`, and sample age in seconds.
  Optional metrics are sparse (`noSuchInstance` when unavailable); never report
  zero for an unsupported sensor. Retained stale samples require stale status.
- Table indexes are positive Integer32 values assigned by a persistent local
  identity registry. Do not recycle them for different entities. Missing or
  reset registry state requires a new inventory epoch and rediscovery.
- GPU identity uses PCI domain/bus/device/function plus device UUID when
  available; a DRM card number alone is not persistent identity. Display identity
  combines GPU identity and connector path; optional hashed EDID identifies a
  monitor replacement. Export no EDID serial or window/client titles by default.
- Disconnected display rows remain with disconnected status for the producer
  lifetime; removed GPU rows remain as absent. Returning identities retain their
  indexes. A display's render GPU and scanout GPU are separate foreign keys.
- Snapshots are immutable for an individual agent request; expose generation and
  producer epoch so collectors can detect changes across a multi-request walk.

These choices follow [SMIv2](https://www.rfc-editor.org/rfc/rfc2578.html) and
[HCNUM-TC](https://www.rfc-editor.org/rfc/rfc2856.html). Formal modules must specify
MAX-ACCESS, ranges, UNITS, INDEX, descriptions, and discontinuities explicitly.

## FOCALDESK-MIB object specification

Relative paths below start at `focalDesk.1`. Scalars append `.0` on the wire.
Tables use `<table>.1.<column>.<index>`; index columns are not-accessible, other
columns read-only. Column order is the proposed allocation, subject to review
before formal MIB publication.

| Path | Object | Syntax / units / meaning |
| --- | --- | --- |
| 1.1 | fdCompositorUptime | TimeTicks; current compositor lifetime |
| 1.2 | fdCompositorEpoch | OCTET STRING (SIZE(16)); changes each start |
| 1.3 | fdWaylandClients | Gauge32; connected client connections |
| 1.4 | fdWaylandSurfaces | Gauge32; existing wl_surface objects |
| 1.5 | fdResidentBytes | CounterBasedGauge64; process RSS bytes |
| 1.6 | fdInventoryGeneration | Counter64; increments on topology changes |
| 2 | fdGpuTable | INDEX fdGpuIndex; device-level state, all vendors |
| 3 | fdDisplayTable | INDEX fdDisplayIndex; connector/output state |

`fdGpuEntry` columns:

| Column | Object | Syntax / meaning |
| --- | --- | --- |
| 1 | fdGpuIndex | Integer32 (1..2147483647) |
| 2 | fdGpuIdentity | OCTET STRING (SIZE(1..128)); canonical device key |
| 3 | fdGpuVendorId | Unsigned32; PCI vendor ID |
| 4 | fdGpuState | INTEGER: ready(1), recovering(2), failed(3), absent(4), unknown(5) |
| 5 | fdGpuDeviceLosses | Counter64; observed VK_ERROR_DEVICE_LOST incidents |
| 6 | fdGpuRebuildAttempts | Counter64; logical-device recreation attempts |
| 7 | fdGpuRebuildSuccesses | Counter64; recreation successfully completed |
| 8 | fdGpuDriverResets | Counter64; optional confirmed driver/hardware resets |
| 9 | fdGpuAllocatedBytes | CounterBasedGauge64; renderer-owned allocation bytes |
| 10 | fdGpuBudgetBytes | CounterBasedGauge64; optional reported Vulkan memory budget |
| 11 | fdGpuDiscontinuityTime | TimeTicks; last counter discontinuity on master clock |
| 12 | fdGpuCapabilities | BITS; deviceLoss(0), rebuild(1), driverReset(2), ownedMemory(3), memoryBudget(4) |
| 13 | fdGpuSampleStatus | INTEGER; shared sample-status enum |
| 14 | fdGpuSampleAge | Gauge32; seconds since producer sample |
| 15 | fdGpuBusyPercent | Gauge32; optional device-wide busy percentage |
| 16, 17 | fdGpuVramUsedMiB, fdGpuVramTotalMiB | Gauge32; optional driver-reported framebuffer use and size in MiB |

GPU rebuilds are not hardware resets. Increment loss once per incident, not once
per failed API call. Allocation-pressure reclamation is not device loss. Track
owned allocations separately from imported DMA-BUF backing; neither is total
system GPU memory usage. Budget is not installed VRAM. The current renderer uses
direct Vulkan allocations; accounting must follow successful allocate/free paths.

`fdDisplayEntry` columns:

| Column | Object | Syntax / meaning |
| --- | --- | --- |
| 1 | fdDisplayIndex | Integer32 (1..2147483647) |
| 2 | fdDisplayIdentity | OCTET STRING (SIZE(1..128)); canonical connector key |
| 3, 4 | fdDisplayRenderGpuIndex, fdDisplayScanoutGpuIndex | Integer32; GPU foreign keys; 0 means unknown |
| 5 | fdDisplayState | INTEGER: active(1), disabled(2), disconnected(3), failed(4), unknown(5) |
| 6, 7 | fdDisplayWidth, fdDisplayHeight | Gauge32; active mode pixels; sparse without mode |
| 8 | fdDisplayRefreshMilliHz | Gauge32; mode refresh in 0.001 Hz |
| 9 | fdDisplayHdrState | INTEGER: sdr(1), hdrActive(2), unsupported(3), unknown(4); actual output state |
| 10 | fdDisplayPresentedFrames | Counter64; completed KMS presentations |
| 11, 12 | fdDisplayRenderCpuTimeTotalUs, fdDisplayRenderSamples | Counter64; accumulated CPU render-submission microseconds and observations |
| 13 | fdDisplayRenderP95Us | Gauge32; approximate p95 over trailing 60 s |
| 14 | fdDisplayRenderWindowSamples | Gauge32; observations in that window |
| 15 | fdDisplayDiscontinuityTime | TimeTicks; last counter discontinuity |
| 16 | fdDisplayCapabilities | BITS; mode(0), hdr(1), presentation(2), cpuRenderTiming(3) |
| 17, 18 | fdDisplaySampleStatus, fdDisplaySampleAge | Shared status enum; Gauge32 seconds |

CPU render-submission timing ends when submission returns; it is not GPU execution
or scanout latency. Hardware timestamp timing needs separate future objects and
capability bits. Compute FPS from presentation counter deltas, per output, rather
than adding outputs into an ambiguous compositor FPS. Publish p95 only with at
least 20 samples; document the chosen histogram buckets before implementation.
Future frame-deadline, audio, Bluetooth, portal, and IPC objects require separate
allocations; do not claim these measurements are available today.

## FOCALVECTOR-MIB object specification

Relative paths start at `focalVector.1`; same scalar/table rules apply.

| Path | Object | Syntax / meaning |
| --- | --- | --- |
| 1.1, 1.2 | fvUptime, fvEpoch | TimeTicks; OCTET STRING (SIZE(16)); daemon lifetime |
| 2 | fvDatabaseTable | INDEX fvDatabaseIndex; one logical collection per row |
| 3 | fvIndexTable | INDEX fvDatabaseIndex, fvIndexIndex; one index per row |

`fvDatabaseEntry` proposed columns, in order:
1 index (Integer32); 2 identity (opaque OCTET STRING, 1..128 bytes);
3 vector count (CounterBasedGauge64, current vectors); 4 dimensions (Gauge32);
5 query completions (Counter64, all outcomes); 6 query failures (Counter64,
subset of completions including timeouts, excluding cancellation);
7 query duration total (Counter64, microseconds, all completed outcomes);
8 duration samples (Counter64); 9/10 query p95/p99 (Gauge32, microseconds,
trailing 60 seconds); 11 window samples (Gauge32); 12 checkpoint failures
(Counter64); 13 last successful checkpoint (DateAndTime, UTC, sparse until success);
14 counter discontinuity (TimeTicks); 15 capabilities (BITS: queries(0), latency(1),
checkpoint(2)); 16 sample status (shared INTEGER enum); 17 sample age (Gauge32,
seconds). Quantiles are sparse below 20/100 samples respectively. Query rate
and mean duration are manager-side deltas; include window counts with quantiles.

`fvIndexEntry` proposed columns, in order:
1 database index and 2 index index (Integer32); 3 identity (opaque OCTET STRING,
1..128 bytes); 4 kind (INTEGER: hnsw(1), flat(2), other(3)); 5 state (INTEGER:
ready(1), building(2), failed(3), unavailable(4)); 6 current RAM bytes and
7 current GPU allocation bytes (CounterBasedGauge64); 8 configured efSearch
(Gauge32, HNSW only; per-query overrides are not represented here);
9 last successful build duration (Gauge32, milliseconds, sparse until success);
10 CUDA enabled (TruthValue); 11 CUDA failures (Counter64); 12 CUDA execution
time total (Counter64, microseconds, completed GPU timings only); 13 counter
discontinuity (TimeTicks); 14 capabilities (BITS: ram(0), gpuMemory(1), hnsw(2),
cuda(3), cudaTiming(4)); 15 sample status; 16 sample age (Gauge32, seconds).
CUDA execution time is sparse without device timing; host wall time must not
silently substitute for it. No collection names, paths, query text, or vectors
are exported. Final field feasibility must be checked against FocalVector.

## Common health, notifications, and conformance

`focalCommon.1.1` is a service table indexed by persistent service index. Proposed
columns: index, product branch number, fixed service kind, producer epoch, health
(`ok, degraded, failed, unknown`), producer uptime, last snapshot age, counter
discontinuity time, dropped event Counter64, inventory epoch (16 bytes).
Service kind identifies compositor, vector daemon, audio, Bluetooth, portal, or
IPC; services without an actual health source remain unknown. Each producer has
its own restart epoch even when the sidecar remains running.

Allocate notification suffixes under each product's `.0` branch:

| Module / suffix | Notification | Payload beyond mandatory sysUpTime.0 and snmpTrapOID.0 |
| --- | --- | --- |
| Desk / 1 | fdGpuDeviceLost | Indexed GPU identity, state, loss count; service epoch |
| Desk / 2 | fdGpuRecoveryChanged | Indexed GPU identity, state, rebuild attempt/success counts |
| Desk / 3 | fdDisplayStateChanged | Indexed display identity, state, HDR state |
| Vector / 1 | fvDatabaseFailed | Indexed database identity and failure count |
| Vector / 2 | fvCheckpointFailed | Indexed database identity and checkpoint failure count |
| Common / 1 | focalServiceHealthChanged | Indexed service kind, health, epoch |

Formal ASN.1 OBJECTS lists must use these defined objects and explicit ordering;
service-epoch payloads reference the matching common service row. Counters and
state snapshots are authoritative even when a notification is lost. Prefer
SNMPv3 INFORM with bounded retries; deduplicate manager events using epoch,
indexed identity, and relevant counter/state. Rate-limit repeated events per
entity (initial proposal: one per 30 seconds), retain latest state, and expose
suppression/drop counts in a later common transport group. A supervisor must
detect producer disappearance; a crashed compositor cannot report its own crash.
Threshold alarms belong in OpenNMS, with hysteresis, rather than per-frame traps.

Define OBJECT-GROUPs for desk base, GPU recovery, displays, optional GPU memory;
vector base, queries, HNSW, optional CUDA, optional persistence; and common health.
Define NOTIFICATION-GROUPs separately. MODULE-COMPLIANCE requires only base and
common-health support initially, with explicit conditional requirements for
supported tables. GPU recovery and display groups are required for the FocalDesk
multi-GPU profile. Unsupported optional columns stay sparse and are documented
in AGENT-CAPABILITIES. Review modules using
[RFC 4181](https://www.rfc-editor.org/rfc/rfc4181.html).

## Delivery sequence and acceptance

1. Review relative assignments, identity lifetime, event definitions, and metric
   availability. Confirm producer sources in FocalVector. Produce formal
   FOCAL-ROOT-MIB, FOCAL-TC-MIB, FOCAL-COMMON-MIB, FOCALDESK-MIB, and
   FOCALVECTOR-MIB templates; validate generated lab modules with an SMI checker
   and Net-SNMP translation. The production root and read-only FocalDesk
   subset are drafted. The shared TC, common, and vector modules remain planned.
2. Implement the transport-independent snapshot/event model. First producers:
   primary/secondary GPU state, device loss and rebuild accounting, display modes,
   presentation counts, and CPU render-submission timings. Hook
   `VulkanDevice`/`SecondaryGpu` lifecycle and KMS completion paths in
   `focaldesk-engine/src/backend/drm_vulkan*`; hold counters outside replaceable
   `AshDrmRenderer` instances. Do not count a rebuild as a driver reset.
3. Build a read-only local AgentX adapter and SNMPv3 lab master. Check GET,
   GETNEXT/GETBULK ordering, sparse columns, Counter64 round-trips, denied SET,
   denied unauthorized access, reconnect, stale producers, bounded event loss,
   and coherent request snapshots. Prove timing overhead fits the compositor's
   frame budget before enabling collection by default.
4. Validate with OpenNMS: discover GPUs/displays by persistent index; collect
   counters, discontinuity and epoch together; derive rates; alarm on recovery
   failures, inactive expected outputs, stale snapshots, and missed frame budgets.
   Exercise one GPU, both GPUs, absent sensors, hot-unplug/replug, index-registry
   reset, process restart, device recreation, notification loss, and agent restart.
   Use injected faults rather than intentionally resetting production hardware.
   In FocalVector, use existing benchmarks to compare counters and latency samples
   against known request totals with CPU/CUDA capability combinations.
5. After PEN assignment, verify the registry, render the root module, rerun SMI
   validation and walk/discovery tests, then publish versioned MIBs and OpenNMS
   collection/event definitions together. Document migration of any lab OIDs.

Acceptance for the first vertical slice: independently identify both render GPUs
and every active output; show a simulated device-loss incident and successful
rebuild without counter reset; distinguish an unavailable sensor from zero;
demonstrate authorized SNMPv3 polling and one recovery notification in OpenNMS.
No runtime services or monitoring endpoints are enabled by this design document.
