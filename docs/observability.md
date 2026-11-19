# Local AI observability

For the proposed shared SNMP namespace, per-GPU/display tables, and staged
FocalDesk/FocalVector implementation, see the
[enterprise MIB design](architecture/enterprise-mibs.md). This is a planning
specification; the SNMP agent is not implemented. The local Vulkan metrics slice
is described below.

The AI console and `focaldesk-server` support opt-in OpenTelemetry traces and operation metrics over
OTLP HTTP/protobuf. Start the local Collector and Jaeger from the repository root:

```sh
docker compose -f tools/observability/compose.yaml up -d
```

Podman with a Compose provider also works. The host ports are bound to loopback:
Collector HTTP on 4318 and [Jaeger UI](http://localhost:16686) on 16686.
Jaeger stores traces in memory; restarting its container clears them.

Verify the complete exporter → Collector → Jaeger path with synthetic data:

```sh
FOCALDESK_OTEL=1 cargo run -p focaldesk-logging --example otel_smoke
```

Select `focaldesk-otel-smoke` in Jaeger. The command prints the trace ID and emits
five spans, including a simulated IPC parent/child relationship.

Enable the rebuilt applications in the environment that starts **both** processes:

```sh
export FOCALDESK_OTEL=1
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
cargo run -p focaldesk-server
# In another terminal with the same environment:
cargo run -p focaldesk-ai-console3
```

For installed systemd services, use service-specific environment overrides and
restart the rebuilt services. No installed binaries or running service environments
are changed by this source integration. Unset `FOCALDESK_OTEL` to disable export.
Service names are `focaldesk-ai-console` and `focaldesk-server`.
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` can specify a full `/v1/traces` URL.
The transport is HTTP/protobuf; this integration does not select gRPC based on
`OTEL_EXPORTER_OTLP_PROTOCOL`.

Ask a question that uses indexed memory. Expected spans include `ai.request`,
`ai_ipc_request`, `ai.chat` or `ai.chat_stream`, `ai.context.assemble`,
`memory.recall`, `ai.embedding`, `memory.search`, `focal_vector.query`, and
`ai.inference` or `ai.inference.stream`. Some stages are skipped when unnecessary.
Inference includes provider retries and stream consumption. Its `outcome` and
OTel status distinguish success, failure, and timeout. Context assembly includes
retrieval; nested durations must not be added together.

The versioned AI IPC envelope carries optional W3C trace context. Existing clients
without context still work, and older servers ignore the optional field. Legacy
bare-payload IPC starts a new server trace. Retrieval explicitly carries its parent
into the blocking worker. Local server request logs include trace/span IDs while
export is enabled. Normal server shutdown on SIGINT/SIGTERM flushes the bounded
batch exporter; forced termination can lose queued spans.

Only spans under the reviewed `focaldesk.telemetry` target are eligible for trace export.
Operation metrics are emitted separately through the SDK. Existing
log events and diagnostic spans are excluded. Instrumentation skips all function
arguments, recording only request IDs, protocol version, vector dimensions, top-k,
and fixed outcome labels. It does not export prompts, documents, vectors,
credentials, URLs, arbitrary errors, or model output. Existing local logging policy
is unchanged. Avoid adding user content as telemetry attributes.

The SDK supports standard sampling variables, for example:

```sh
export OTEL_TRACES_SAMPLER=parentbased_traceidratio
export OTEL_TRACES_SAMPLER_ARG=0.1
```

FocalVector's sibling repository now supports optional W3C context in its local
IPC envelopes and exports `focal_vector.request`, `focal_vector.search`,
`focal_vector.hnsw.search`, and `focal_vector.cuda.search` spans. The daemon carries
context through its blocking worker. CUDA spans appear only when that backend is
built and used; ordinary CPU retrieval does not produce GPU spans.

Until these client changes are published upstream, use the explicit local build
wrapper so Cargo uses `../focal-vector/crates/focal-vector-client` instead of the
pinned upstream client. This avoids silently changing the published dependency:

```sh
(cd ../focal-vector && cargo build --release --bin focal-server)
(cd apps/focaldesk-ai-console3 && npm run build)
tools/observability/build-local.sh build -p focaldesk-server -p focaldesk-ai-console3 -p focaldesk-cli
python3 tools/observability/activate-local.py       # show overrides
python3 tools/observability/activate-local.py --apply
```

Activation writes `90-local-observability.conf` under the user service drop-in
folders for `focal-vector`, `focaldesk-server`, and `focaldesk-ai-console-app`, then
restarts them. It uses the optimized vector daemon and development builds of the
AI server/console, leaving packaged binaries intact. The vector override permits
AF_INET/AF_INET6 alongside AF_UNIX because the packaged unit only permits Unix
sockets and OTLP export needs loopback TCP. Activation starts the supervised stack and waits for readiness before restarting
the application services. These drop-ins persist across logins and reference this checkout.
The CLI also supports `FOCALDESK_OTEL=1` for reproducible live request validation:

```sh
FOCALDESK_OTEL=1 target/debug/focaldesk-cli ai chat --memory 'Find documentation about Vulkan HDR rendering.'
```

To return to packaged binaries and disable tracing, remove only the three
`90-local-observability.conf` files created by activation, run
`systemctl --user daemon-reload`, and restart those three services. Then run
`systemctl --user disable --now focaldesk-observability.service` to disable stack
autostart. Other user service overrides remain in place.

Vendor hardware sensors, OTel log export, and a native FocalDesk dashboard remain
future work. The Vulkan compositor supports the opt-in local snapshot below.

Stop the supervised local stack with:

```sh
systemctl --user stop focaldesk-observability.service
```

Use Compose directly only when the supervisor is stopped. While it runs, it will
restart a stack whose containers exit. Named Prometheus/Grafana volumes survive
stops. Application service restarts can start the stack again through their Wants
dependencies.

Implementation follows the [Rust OTLP exporter documentation](https://docs.rs/opentelemetry-otlp/0.31.1/opentelemetry_otlp/).

## Verified local request

On 2026-11-10, the rebuilt user services were activated with the local client
override. A live CLI request asked for Vulkan HDR documentation, retrieved five
sources, and completed with `gemma4:12b`. Jaeger trace
`36c18e6014c01b3beb9265d2300f2452` linked `focaldesk-cli` → `focaldesk-server` →
`focal-vector`, including both IPC parent links and successful inference status.
The request took 10.32 s, including 1.48 s embedding, a 4.63 ms FocalVector client
call (2.86 ms daemon search), and 8.74 s inference. These are observed durations
for one request, not benchmark results. HNSW/CUDA spans appear only on searches
that use those paths; neither appeared in this request.

After making a live CLI request, check the trace with:

```sh
python3 tools/observability/verify-live.py
```

This verifies the three service identities, the two cross-service parent links,
and inference success. It prints timing metadata without printing source contents.
Jaeger data is ephemeral and this trace link will disappear if its container restarts.

## Request metrics dashboard

Open [FocalDesk AI Operations](http://localhost:3000/d/focaldesk-ai/focaldesk-ai-operations).
Grafana is bound to loopback on port 3000 with anonymous viewer access. Prometheus
is bound to loopback on port 9090. Provisioning includes the data source, service
and stage filters, and 18 panels:

- Exporter freshness, heartbeat age, and Collector scrape status.
- Completed observation counts in the selected range.
- Median and p95 latency estimates over that same range.
- Active-operation counts and oldest active-operation age.
- Streaming first-content latency and output tokens per second.
- Ollama model-loading, prompt-processing, and generation timings, plus generation-only tokens per second.
- Completed operations per second, error fraction, timeouts, and cancellations.
- Completed counts in the selected range alongside process-lifetime totals.

The Collector exports metrics internally to Prometheus, which scrapes every five
seconds and retains data for 15 days in a named volume. Grafana state also uses a
named volume. Stopping the stack with `compose down` preserves these volumes.
User-service activation sets `OTEL_METRIC_EXPORT_INTERVAL=5000`; otherwise the SDK
uses its default export interval (60 seconds). The same `FOCALDESK_OTEL=1` or
`FOCAL_VECTOR_OTEL=1` switch enables both signals, and shutdown flushes both.

The instruments are `focaldesk.operation.requests` (counter) and
`focaldesk.operation.duration` (seconds histogram). In Prometheus these become
`focaldesk_operation_requests_total` and
`focaldesk_operation_duration_seconds_{bucket,count,sum}`. The only operation
labels are fixed `stage` and `outcome` enums; the Collector adds service/resource
metadata. No prompt, source path, vector, user ID, model output, or error message
is used as a metric label. Metrics do not depend on trace sampling.

Stages are `request` (chat and streaming chat, excluding administrative IPC),
`embedding` (one Ollama batch call), `inference` (one managed provider call,
including retries and stream consumption), and `vector_search`. The latter is
recorded at both the AI server's client boundary and the FocalVector daemon, so
compare them by service instead of summing their counts. Request latency also
contains embedding/retrieval/inference; these nested durations must not be added.

Each completed operation records one counter increment and one histogram
observation. Outcomes are `ok`, `error`, `timeout`, and `cancelled`. Dropping an
unfinished operation counts as cancellation; explicit stream cancellation and
stream-size failure retain their own outcomes even when the IPC method returns
successfully after sending a terminal event. Typed provider, embedding HTTP, and
vector-client socket timeouts are distinguished from ordinary errors. A timeout
is a failure in the error-rate panel; cancellation is separate.

Percentiles are bucket-based estimates, not exact samples. Median and p95 use the
selected dashboard range. p95 is hidden below 20 completed observations and shows
“Limited data (<20)”; the count panel provides the sample context. Twenty is a
visibility threshold, not a guarantee of statistical confidence. Counter increases
are reset-aware estimates and may be fractional because Prometheus extrapolates
between scrapes. Real zero counter baselines are exported at startup; histogram
samples are never seeded with fabricated zero latencies. Charts still need traffic
across multiple scrapes. Counts reset when a process restarts. Synthetic smoke
metrics are excluded from the dashboard's service selection.

Validate the metrics pipeline independently of tracing:

```sh
FOCALDESK_OTEL=1 OTEL_TRACES_SAMPLER=always_off cargo run -p focaldesk-logging --example metrics_smoke
# After real --memory chat requests and a scrape/export interval:
python3 tools/observability/verify-metrics.py --smoke
```

The smoke emits one of each outcome under `focaldesk-metrics-smoke`, without
calling a model or reading documents. With `--smoke`, the verifier also checks
those outcomes. Its normal checks cover live metrics for all four AI stages and
the vector daemon, matching histogram counts, streaming data, and the queries
from Grafana's provisioned dashboard. On 2026-11-10 these checks
passed after live local documentation requests. The 139 AI unit tests, 15 memory
unit tests, ten logging/telemetry unit tests, and 84 FocalVector workspace unit
tests also passed with this integration.

## Health, sparse data, streaming, and automatic startup

`focaldesk-observability.service` is enabled under the user's `default.target`, so
it starts at user login. `activate-local.py --apply` installs it along with Wants
and After dependencies in the three application overrides. No root service or
linger setting is required. Its supervisor runs Compose in the foreground, waits
for Collector health (loopback port 13133), Prometheus readiness, Grafana health,
and Jaeger's API, then signals systemd readiness. Application restarts are ordered
after this readiness signal. The supervisor stops containers on shutdown without
removing volumes. It exits on an unexpected Compose exit or three consecutive
readiness failures, allowing systemd to retry after ten seconds. This supervision
does not make in-memory Jaeger traces durable across container restarts.

```sh
systemctl --user is-enabled focaldesk-observability.service
systemctl --user status focaldesk-observability.service
journalctl --user -u focaldesk-observability.service
```

Every enabled SDK exports a Unix timestamp heartbeat on each metric collection,
even with no requests. The freshness recording rule expects the AI server, vector
daemon, and native console; it reports stale/missing when heartbeats are 20 seconds
old, absent, or the Collector scrape fails. Collector-cached metrics therefore
cannot keep a stopped exporter looking fresh. Heartbeat freshness means the SDK
collection/export path is alive; it does not prove application event-loop health
or inference readiness. Active operations and their oldest elapsed age help reveal
work that is stuck despite a fresh heartbeat. Those gauges are hidden when their
service's telemetry is stale, rather than showing old counts as current.

Active counts and oldest age use a bounded registry keyed internally by operation
ID; only fixed stage labels are exported. Entries are removed on completion,
cancellation, and future drop. Zero means idle when the exporter is fresh. Nested
operations overlap; do not add their counts to obtain a user-request count.

Streaming `focaldesk.inference.first_content` measures provider-call start to the
first nonempty content chunk, including startup and retries. It records once per
stream that produces content, including streams that later fail or cancel. This is
a provider-side proxy for time to first token, not UI-perceived latency; retrieval
and final IPC delivery are outside it. Empty chunks do not trigger the observation.

Streaming `focaldesk.inference.output_tokens` uses final provider-reported token
usage only. Successful streams with a positive token count also record
`focaldesk.inference.stream_duration`, including startup. The dashboard divides
summed reported tokens by summed stream durations over the selected range. This
is end-to-end output throughput, not raw GPU decode speed. Missing token usage is
omitted rather than estimated from character or chunk counts; failed/cancelled
streams do not contribute successful-stream throughput.

To exercise and verify streaming panels, make two successful streamed requests,
allowing export/scrape time between them:

```sh
FOCALDESK_OTEL=1 target/debug/focaldesk-cli ai chat --memory --stream 'Summarize the indexed HDR documentation.'
# Wait for an export/scrape interval, then repeat a streamed request.
python3 tools/observability/verify-metrics.py
```

The verifier checks all 18 dashboard queries, three expected fresh exporters,
streaming measurements, and sparse-p95 gating. The health-rule fixtures exercise
fresh, stale, missing-exporter, and failed-scrape cases without stopping real
services:

```sh
docker compose -f tools/observability/compose.yaml exec -T prometheus promtool test rules /etc/prometheus/health-tests.yaml
```

Latest validation observed active-request metrics during two real streamed HDR
documentation requests. All 18 panel queries passed, including the below-20-sample
p95 guard; all three exporter freshness rules were healthy. The user service was
confirmed enabled and ready, and the three application services were confirmed
running with the expected Wants/After dependencies. The initial Compose handoff
needed one automatic retry; systemd supervision recovered it. No reboot was
performed for this validation.

## Warm Ollama inference and phase timings

FocalDesk sends `keep_alive: "20m"` on Ollama chat, structured-planning, and
streaming requests. Each use renews a 20-minute idle residency window for that
model. This is a per-request FocalDesk policy; Ollama's global settings are not
changed. Idle expiry, server restart, or memory pressure can still cause a later
cold load. Keeping a model resident retains its RAM/VRAM allocation.

`FOCALDESK_OLLAMA_KEEP_ALIVE` configures the policy. Supported values are positive
integer durations such as `500ms`, `30s`, `20m`, or `1h`; `0` requests immediate
unload and skips startup preloading, and `-1` explicitly requests indefinite
residency. The default is `20m`. Invalid values fail configuration clearly.

The activated and packaged server units also set `FOCALDESK_OLLAMA_PREWARM=1`.
At startup, the server asynchronously preloads the selected default provider's
model using Ollama's empty `/api/generate` request. It sends no user prompt,
document, or chat message. The model is resolved using the same configured/default
selection as ordinary chat. Hosted providers use a no-op warm-up. The preload
runs in the background, so IPC/UI startup stays responsive. Its failure is logged
and ordinary requests remain available. An immediate request can still arrive
before warm-up finishes. Set `FOCALDESK_OLLAMA_PREWARM=0` to disable startup loading.
Provider default/model changes take effect on the next server start or request;
there is no timer continually reloading unused models.

Change these environment settings through the server environment file or a later
user-service override and restart `focaldesk-server.service`. The packaged server
units and local activation script include the same 20-minute/prewarm defaults.
Other applications' Ollama requests can affect model residency.

Ollama's optional `load_duration`, `prompt_eval_duration`, and `eval_duration`
response fields are collected for completed provider attempts in both streaming
and non-streaming modes. Streaming timings are taken once from the terminal event.
Missing fields produce no timing sample; reported zeros remain valid samples.
Nanoseconds are converted to seconds. Warm-up-only requests are excluded from
chat phase metrics so startup loads do not inflate request timing charts.

The instruments are:

- `focaldesk.ollama.phase.duration`, with fixed `phase` values `model_load`,
  `prompt_eval`, and `generation` (seconds histogram).
- `focaldesk.ollama.generated_tokens`, paired with
  `focaldesk.ollama.generation_time` (seconds counter), for responses reporting
  positive token counts and generation durations.

The dashboard's phase bars show selected-range arithmetic means of these reported
durations. Generation-only throughput divides paired token counts by paired
generation time, excluding loading and prompt processing. This differs from the
end-to-end streaming throughput panel, which includes startup. These provider
phases are not invented child-span wall times and need not sum to measured client
wall time, which also contains scheduling, transport, retries, and parsing.
Numeric phase timings and output-token counts are also attached to the managed
inference span when available. No prompt, response text, or model-name metric
labels are added. Metrics aggregate completed Ollama attempts across models used
by that service; they are not a controlled per-model benchmark.

The implementation follows Ollama's [chat API](https://docs.ollama.com/api/chat)
and [generation API](https://docs.ollama.com/api/generate) contracts.

Validation after activating this policy confirmed a completed background warm-up
and a `gemma4:12b` residency deadline 20 minutes after preloading. Two real streamed
HDR documentation requests then reported model loading of 0.000906 s and
0.000891 s. Prompt processing was 0.980–0.988 s, and generation was 5.36–6.69 s
for 1,000–1,257 reported tokens. Total measured inference wall time was
6.71–7.70 s. These were real RAG requests, not a controlled before/after speed
benchmark; the result confirms negligible warm loading cost while identifying
token generation as the largest remaining component. Both request traces carried
the phase fields, all 18 dashboard queries passed, and 139 AI plus ten
logging/telemetry unit tests passed.


## Local Vulkan GPU and display snapshots

The `drm-vulkan` compositor now collects process-lifetime per-GPU device-loss
incidents, logical-device rebuild attempts/successes, and per-connector completed
KMS presentation counts. These counters live outside `AshDrmRenderer`, survive
renderer and output replacement, and remain in a process-local registry across
secondary GPU removal/re-addition. They reset on compositor restart; snapshots
include a producer epoch, uptime, schema version, and wall-clock sample timestamp.
This slice exports JSON independently of OTLP; it does not add SNMP or hardware
sensor polling.

To enable export, set this variable in the environment that launches a newly
built Vulkan compositor (use the existing private user runtime directory):

```sh
export FOCALDESK_METRICS_PATH="$XDG_RUNTIME_DIR/focaldesk-vulkan-metrics.json"
```

The destination must be absolute and its parent must already exist. This setting
is independent of `FOCALDESK_OTEL`; unset it to disable file export. No current
service is restarted or configured by this source change. Inspect the snapshot:

```sh
python3 -m json.tool "$XDG_RUNTIME_DIR/focaldesk-vulkan-metrics.json"
```

A worker writes owner-only files via atomic rename, about once per second while
normal event-loop progress continues. Rendering never waits for the writer; a
bounded queue drops snapshots under backpressure and increments
`dropped_snapshots`. Failed recovery requests an immediate best-effort snapshot;
process termination can still lose queued data. The file remains after exit, so
check `sample_unix_ms` for freshness and `producer_epoch` for restarts. This file
is for local inspection, not a durable event log or a crash-reporting mechanism.

`gpus` is keyed by canonical sysfs device path (PCI topology on PCI devices), with
an explicitly recognizable `drm:major:minor` fallback if identity resolution fails.
Nested `displays` use connector names scoped to their GPU; primary and secondary
names cannot collide. Identity/index persistence across compositor restarts and
UUID-based identity are future work for the SNMP inventory model.

GPU states are `ready`, `paused`, `failed`, or `absent`. A device-loss incident
means a typed `VK_ERROR_DEVICE_LOST` error, deduplicated until a replacement device
is created. Allocation pressure and suspend/resume rebuilds do not increment loss
counts. Rebuild success means logical-device creation succeeded, independently
of later scanout initialization. These are not driver/hardware reset counters.

Each configured display reports width/height and selected mode refresh in
millihertz (currently derived from DRM's integer-Hz mode value). Test-pattern
outputs report unknown refresh as JSON `null`. Display state is `active`,
`inactive` (paused or failed device), `failed` (an output-specific incident),
`disabled` (saved configuration), `disconnected` (DRM connector or GPU
removed), or `notConfigured` (no output with a usable mode). Retired rows
retain counters but clear mode fields to `null`. Connected output incidents
count when KMS presentation or rendering completion fails or times out.
The first completed presentation afterward counts as recovery. Unplugging
or disabling a failed connector clears the incident without counting recovery. Presented-frame counts
advance only after successful KMS page-flip completion for an outstanding frame,
not on render submission or cleanup. Compute observed per-output FPS from count
and sample-time deltas within one producer epoch.

This first slice does not expose GPU memory/utilization, HDR state, CPU/GPU render
latency, or percentile estimates. These require additional measurements and
capability contracts before the SNMP adapter is implemented.

Verify without a running compositor or real device reset:

```sh
cargo test -p focaldesk-engine --features drm-vulkan --offline backend::drm_vulkan --lib
```

### Persistent monitoring inventory

Read the snapshot and assign stable, positive Integer32 indexes with:

```sh
python3 scripts/focaldesk-monitor-inventory.py
```

The reader emits separate GPU and display rows. Each display references its
scanout GPU by index, and its identity combines the snapshot GPU key and
connector name. Indexes persist in
`${XDG_STATE_HOME:-$HOME/.local/state}/focaldesk/monitor-inventory.sqlite3`
across compositor restarts and temporary removal. If the database is reset,
the inventory epoch changes and consumers must rediscover indexes. The
compositor's producer epoch independently marks counter resets.

Sample age is derived from the exporter timestamp. Rows become `stale` at five
seconds by default, or at the threshold set by `--stale-after`. A future
timestamp yields `unavailable` with a null age. Missing or unreadable snapshots
produce an error and no inventory output. Use `--snapshot` and `--registry`
to inspect a capture with a separate registry.

### Input dispatch delay

The opt-in Vulkan snapshot includes `input_dispatch_delay` for keyboard
events (presses and releases), pointer motion, and pointer buttons. Each
series contains the process-lifetime count, total microseconds, maximum, and
histogram bins. The timer measures from the libinput event timestamp to the
start of the compositor callback. It does not include client delivery or
display presentation. Libinput may backdate synthesized touchpad clicks,
which is why button timing is reported separately from pointer motion.

Record a snapshot before and after each trial while performing comparable
typing and pointer motion. Wait for a fresh snapshot before recording
the end of a trial. Compare samples within the same `producer_epoch`.
Subtract both the count and total across snapshots to compute interval mean
delay. Subtract each histogram bin to compare the proportion over 8 ms,
16 ms, and 32 ms, or to estimate a percentile. Bin edges are inclusive
upper bounds, and the last bin contains values above 128 ms. The maximum
is cumulative, so use histogram deltas to compare trial intervals.
The metrics contain no key codes or pointer coordinates.
