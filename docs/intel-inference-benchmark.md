# Intel embedding inference benchmark

This experiment compares a pinned `sentence-transformers/all-MiniLM-L6-v2`
OpenVINO export on the i9 CPU and an explicitly selected Intel GPU. It does not
change the production embedding provider or install system driver packages.

## Setup (Fedora x86_64)

Run from the repository root, outside a filesystem sandbox that hides `/dev/dri`:

```sh
bash scripts/prepare-intel-inference-runtime.sh
source tmp/intel-inference/environment.sh
tmp/intel-inference/venv/bin/python scripts/intel-embedding-benchmark.py \
  --download --model-dir tmp/intel-inference/model \
  --output tmp/intel-inference/results/report.json \
  --gpu GPU.0 --connector HDMI-A-1 --seconds 30
```

`uv`, `dnf download`, `rpm2cpio`, and `cpio` are needed for setup. The frame probe
uses the system Python with PyGObject, GTK4, GTK4 Layer Shell, and pycairo. Check
that the Intel Vulkan manifest in `environment.sh` exists. Use that environment
only in the benchmark shell, not the session-wide environment.

The setup downloads Fedora Intel OpenCL/IGC dependencies, extracts them under
`tmp/`, creates SONAME links normally provided by RPM triggers, and prepares an
isolated Python 3.12 environment. It does not run RPM installation scripts or
replace NVIDIA/Intel display drivers. Downloaded driver package versions are
resolved by Fedora repositories; preserve the RPMs with benchmark artifacts.

The model and tokenizer are pinned to revision
`1110a243fdf4706b3f48f1d95db1a4f5529b4d41`. Only the OpenVINO IR and tokenizer
are downloaded; no remote model code is executed. The default workload is batch
one, padded to 128 tokens, with FP32 inference requested on both devices.

## Measurements and limits

The report identifies available devices and the compiled model's actual
`EXECUTION_DEVICES`. GPU runs fail if the explicitly selected device is not
Intel, is unavailable, or reports execution on a CPU. There is no automatic
fallback. Ten warm-up inferences precede each measured phase. Timing includes
inference, masked mean pooling, and L2 normalization; it excludes tokenization,
model compilation, and network access. CPU and GPU vectors are compared by
cosine similarity, and each embedding must have 384 finite components.

With `--connector`, a 24-pixel input-transparent animated overlay is placed on
that exact monitor. It uses Intel Vulkan and reports GTK frame callback intervals
for idle, CPU inference, and GPU inference. It checks that a layer-shell window
was created and fails rather than accepting an ordinary desktop window as an
output-specific probe. Its layer-shell library is preloaded to support Python GI.

These intervals measure scheduling of a separate GTK client on the same output,
not the companion's own frame times, physical presentation, or input-to-display
latency. Background activity, GPU clocks, thermal state, and phase ordering can
change results. The probe itself introduces continuous output damage. Confirm
visible companion behavior and touch manually while measuring.

Repeat with `--gpu-first` and a different output directory to check ordering
variation. Reports are written incrementally and marked `complete: true` only
after both devices finish. Do not interpret an incomplete report as a successful
GPU benchmark. Optional GPU inference should remain disabled in production until
results justify a workload policy.

## UHD 770 / i9-13900K measurements, 2026-11-15

Hardware: Intel UHD Graphics 770 (`GPU.0`), Intel Core i9-13900K (`CPU`),
RTX 4090 present as `GPU.1` and unused by the explicit inference phases. The
companion service was configured for Intel Vulkan and stayed active throughout.
OpenVINO 2025.4.0, FP32 requested, batch one, sequence length 128, 30 seconds per
measured phase. The model produces 384-dimensional normalized embeddings.

The CPU-first run measured:

| Phase | Inference median | Inference p95 | Embeddings/sec | Frame callback median | Frame callback p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Idle probe | — | — | — | 50.02 ms | 63.22 ms |
| CPU | 4.52 ms | 5.24 ms | 217.71 | 36.25 ms | 51.08 ms |
| UHD 770 | 7.46 ms | 10.71 ms | 122.92 | 35.65 ms | 50.08 ms |

The GPU compiled model reported `EXECUTION_DEVICES=["GPU.0"]`; its full device
name was `Intel(R) UHD Graphics 770 (iGPU)`. CPU/GPU embedding cosine similarity
was 0.9999999999997589. These results establish actual Intel GPU inference and
correct output for this input; they do not measure retrieval quality.

The probe did not show additional pacing degradation under Intel inference
relative to idle. Idle pacing was already slow and variable, and improved during
both inference phases. Clock/power-state effects are a possible explanation,
not an established cause. These data do not establish 60 Hz rendering or touch
responsiveness. CPU inference was faster for this tested FP32 workload; keep the
production embedding provider unchanged rather than automatically assigning it
to Intel on the basis of GPU availability alone.

Raw CPU-first results: [JSON](benchmarks/2026-11-15-intel-embedding-cpu-first.json).
Downloaded compute packages: [versions](benchmarks/2026-11-15-intel-compute-packages.txt).

The GPU-first repeat confirmed the same result:

| Phase | Inference median | Inference p95 | Embeddings/sec | Frame callback median | Frame callback p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Idle probe | — | — | — | 49.86 ms | 61.90 ms |
| UHD 770 | 7.50 ms | 10.77 ms | 121.95 | 35.23 ms | 49.98 ms |
| CPU | 4.57 ms | 5.32 ms | 215.11 | 36.02 ms | 51.50 ms |

The companion retained PID 768929 and `NRestarts=0` across both runs. No new
cross-GPU copy or secondary timeout/failure messages were found during the
measured period. The CPU completed approximately 1.77 times as many embeddings
per second as the Intel GPU in either ordering.

Raw GPU-first results: [JSON](benchmarks/2026-11-15-intel-embedding-gpu-first.json).

The runtime configuration follows OpenVINO's documented
[GPU device selection](https://docs.openvino.ai/2026/openvino-workflow/running-inference/inference-devices-and-modes/gpu-device.html)
and [latency hints](https://docs.openvino.ai/2025/openvino-workflow/running-inference/optimize-inference/optimizing-latency.html).
