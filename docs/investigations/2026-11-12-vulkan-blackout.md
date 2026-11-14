# Vulkan compositor blackout investigation — 2026-11-12

## Fix status

The recovery-lifetime defect is now corrected in the working tree. The compositor
was built but has **not** been installed or restarted. No display modes were
changed. Historical evidence still cannot identify the exact original allocation
or prove that this leak alone caused the first allocation failure.

Allocation errors now retain live scanout, keep Wayland/input dispatch running,
and schedule nonblocking cache reclamation after submitted work completes. Failed
frames retry after 100 ms, then 500 ms, then at most once every five seconds until
a successful frame resets the incident. Memory/pool errors are classified from
the typed Vulkan error chain, including errors wrapped in allocation context.
They no longer trigger unconditional device abandonment/replacement.

Healthy device recreation (including resume) gates teardown with an empty queue
submission and a finite five-second fence wait. The embedded VUI renderer is
dropped after the shared queue drains; the owning renderer releases its resources,
logical device and instance **before** allocating a replacement. A failed or
unresponsive device latches a process-exit-only state. Resume cannot repeatedly
retry a destroyed device or accumulate abandoned replacements. Normal owning
renderer teardown also uses the finite fence; embedded teardown is only entered
after that drain succeeds.

Frame construction now has one ownership batch. Before submission, errors clean
up command buffers, staging/mesh/readback buffers, framebuffers and sync objects.
Retired resources still referenced by earlier submissions move onto their fence.
Successful submission transfers the batch and client-buffer retentions to the
pending-frame queue before exporting the KMS sync file; export failure cannot free
in-flight resources. Texture/LUT uploads are reusable only once submitted. Partial
image/staging and pipeline construction are rolled back. Retired borrowed VUI
descriptors are freed once outstanding frames finish instead of accumulating for
the entire session. Cache reclamation clears dependent LUT descriptors and scene
images together, avoiding stale references if Vulkan reuses a handle.

## Original finding

The desktop session exited after a Vulkan allocation error and an unsuccessful
in-process recovery attempt. This was not a physical display trial initiated by
Settings. The exact original allocation and historical GPU free-memory budget
were not recorded, so VRAM exhaustion, a driver allocation constraint, and an
allocation-count/resource-lifetime issue cannot yet be distinguished.

A concrete recovery defect was present: `recover_vulkan_renderer` always calls
`abandon_device`, including for allocation failures and ordinary session resume.
`AshDrmRenderer::drop` then skips every Vulkan resource/device/instance destructor.
That path was designed for process exit after a hung device, but recovery keeps
the process alive. Consequently abandoned device resources remain allocated
until process exit, and the replacement must be created alongside them. This was
a confirmed resource-lifetime problem and a plausible contributor to allocation
pressure; it is not proof of the original allocation's cause.

## Evidence and timeline

Local timezone: America/New_York (UTC−05:00). Persistent log:
`~/.cache/focaldesk/focaldesk.log`; journal boot
`bb0bb1f4eba0417db118288668b0e136`, compositor PID 2606.

- 08:15:28: a resume recovery already recreated the raw Vulkan device in this
  same session, using the abandonment path. Output retirement/setup followed.
- 13:49:57.873905: `recreating raw Vulkan device and scanout: A device memory
  allocation has failed`.
- No subsequent output-retirement message appears. Since output retirement is
  logged during rebuild, failure before output rebuild, most likely while
  constructing the replacement device, is the strongest source/log inference.
  The exact replacement error was lost; it is not proven by the old log.
- 13:49:58.137732: Smithay reports `Failed to restore previous state. Error:
  Invalid argument (os error 22)`.
- 13:49:58: the login helper closes the session and exits with status 1.
- 13:51:09: logind receives a short power-button press; the OS performs an
  orderly shutdown. It was still processing events after the screens went blank.
- No NVIDIA Xid, kernel GPU hang, kernel panic or OOM-kill was found in that boot's
  kernel journal. This does not rule out a Vulkan/driver allocation failure.

The Smithay restore message is emitted by `AtomicDrmDevice::drop` while restoring
pre-session KMS state. It is secondary shutdown cleanup, not evidence that the
Settings trial rollback mechanism failed. Smithay explicitly documents that a
failed restoration can leave a black screen until another display handler takes
control.

A read-only `nvidia-smi` check during investigation found RTX 4090, 24,564 MiB
reported total, 2,857 MiB used, 21,304 MiB free, 35°C. Those are current readings
after reboot, not telemetry from the failing session. Ollama and other GPU users
were present in the old session, but no contemporaneous per-process VRAM sample
was available. No particular application can be blamed from this evidence.

## Code paths

- `crates/focaldesk-engine/src/backend/drm_vulkan.rs`:
  `render` failure → `recover_vulkan_renderer` → abandon VUI/renderer → construct
  replacement → rebuild outputs. Recovery errors propagate out of the main loop.
- `crates/focaldesk-render/src/ash_drm.rs`:
  `abandon_device` sets `abandon_on_drop`; `Drop` returns immediately when set.
  Successful in-process replacement does not close the abandoned logical device.
- Smithay pinned source:
  `src/backend/drm/device/atomic.rs`, `AtomicDrmDevice::drop`, emits the restoration
  error on a failed `atomic_commit` of the pre-session state.

The Vulkan allocation error alone does not prove every byte of VRAM was full.
Allocation size, allocation-count limits and implementation constraints matter.
See the [Vulkan memory allocation specification](https://docs.vulkan.org/spec/latest/chapters/memory.html).
Correct device teardown also requires freeing device children after outstanding
work has finished; simply deleting the early return risks an unbounded driver
wait or destroying resources still in use. See
[Vulkan device lifetime](https://docs.vulkan.org/spec/latest/chapters/devsandqueues.html).

## Diagnostic additions

The investigation initially added diagnostics. These remain alongside the
recovery fix; no installed binary, daemon or running session was changed:

- Allocation context: operation, requested bytes, memory type/flags, heap index
  and heap capacity, plus owned-image dimensions/format. Heap capacity is clearly
  distinguished from a current free-memory budget.
- Recovery resource counts: imported targets, cached textures, linear scenes,
  output resources, pending frames and retained staging/texture counts.
- Explicit error-chain logging for replacement-device creation and output
  rebuild, before shutdown cleanup can obscure the original recovery failure.
- A warning when an unresponsive device requires process exit instead of another
  in-process replacement.

## Validation of the fix

- Renderer: **39 tests passed**, one opt-in software-driver test ignored in the
  default run. `/tmp/gpu-fix-render-tests.log`.
- The opt-in test was run separately with Mesa Lavapipe explicitly selected:
  **passed**, 32 real Vulkan device/queue create-drain-destroy cycles, CPU driver
  only, no display/KMS access. `/tmp/gpu-fix-software-vulkan.log`.
- Engine: **283 tests passed**, one existing test ignored. Kernel-fence tests ran
  outside the sandbox; no compositor or live monitor was started.
  `/tmp/gpu-fix-engine-tests.log`.
- DRM/Vulkan Clippy with warnings denied passed. `/tmp/gpu-fix-clippy.log`.
- The DRM/Vulkan/Xwayland compositor binary links successfully.
  `/tmp/gpu-fix-desktop-build.log`.
- Formatting and whitespace checks passed. `/tmp/gpu-fix-format.log`.

The new mock-driver tests run the real Vulkan allocation helpers and frame-build
code with injected allocation/mapping failures. They verify preservation of the
previous target and usable device, exact cleanup of failed builds, deferred
ownership of submitted/retired resources, cleanup after partial pipeline creation,
finite waits, timeout behavior, no retry of a destroyed device, and release-before-
replacement across 64 simulated recreation cycles. Engine tests verify retry
backoff. These are regression checks, not physical suspend/resume or KMS recovery
certification.

Reproduce from the repository root:

```sh
cargo test -p focaldesk-render --features ash-drm --lib
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json cargo test -p focaldesk-render --features ash-drm software_vulkan_repeated_queue_lifecycle -- --ignored
cargo test -p focaldesk-engine --features drm-vulkan --lib
cargo clippy -p focaldesk-engine --features drm-vulkan --all-targets -- -D warnings
cargo build -p focaldesk-desktop --no-default-features --features drm-vulkan,xwayland
```

The software ICD filename is distro-specific. Select a CPU driver explicitly;
the opt-in test rejects a device that is not reported as a CPU implementation.

## Remaining release gates

- Record GPU budget/allocation counters during a dedicated-session soak test,
  with/without model workloads and repeated physical suspend/resume. Verify old
  device allocations actually disappear and allocation failures preserve a usable
  desktop on NVIDIA and other supported drivers.
- Verify display-manager takeover/greeter recovery when an unresponsive device
  forces compositor termination. Do not depend on restoration of obsolete
  pre-session KMS state.
- Test physical KMS/HDR changes and recovery in a separate recovery-capable
  session before certifying disruptive monitor testing on the main desktop.

The fix addresses confirmed lifetime and retry defects. It does not establish the
historical VRAM budget, reproduce the original driver failure, or certify all
upstream embedded-renderer/hardware hang paths.
