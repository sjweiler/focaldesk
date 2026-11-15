#!/usr/bin/env python3
"""Compare explicit OpenVINO CPU/GPU MiniLM inference with an optional GTK pacing probe."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import subprocess
import time

MODEL_ID = 'sentence-transformers/all-MiniLM-L6-v2'
REVISION = '1110a243fdf4706b3f48f1d95db1a4f5529b4d41'
FILES = ['openvino/openvino_model.xml', 'openvino/openvino_model.bin', 'tokenizer.json']
TEXTS = [
    'Find the document about independent Intel rendering in FocalDesk.',
    'The companion dashboard displays system controls on a touchscreen.',
    'Background embedding inference should leave the desktop responsive.',
    'Search local project notes for Vulkan synchronization and GPU allocation.',
]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--model-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--download', action='store_true')
    parser.add_argument('--gpu', default='GPU.0')
    parser.add_argument('--gpu-first', action='store_true', help='Reverse phase order to check ordering effects')
    parser.add_argument('--seconds', type=float, default=30)
    parser.add_argument('--sequence-length', type=int, default=128)
    parser.add_argument('--batch-size', type=int, default=1)
    parser.add_argument('--connector', help='Enable Intel GTK frame probe on this connector')
    parser.add_argument('--probe-python', default='/usr/bin/python3')
    args = parser.parse_args()
    if args.seconds <= 0 or args.batch_size < 1 or args.sequence_length < 2:
        parser.error('seconds, batch size and sequence length must be positive')
    if args.download:
        from huggingface_hub import hf_hub_download
        for filename in FILES:
            hf_hub_download(MODEL_ID, filename, revision=REVISION, local_dir=args.model_dir)
    import numpy as np
    import openvino as ov
    from tokenizers import Tokenizer
    args.output.parent.mkdir(parents=True, exist_ok=True)
    core = ov.Core()
    report = dict(started_utc=datetime.now(timezone.utc).isoformat(), model=MODEL_ID, revision=REVISION, openvino=ov.__version__,
                  platform=platform.platform(), batch_size=args.batch_size,
                  sequence_length=args.sequence_length, inference_precision="f32",
                  tokenization_included=False, warmup_iterations=10,
                  devices={d: core.get_property(d, 'FULL_DEVICE_NAME') for d in core.available_devices},
                  phases=[], complete=False)
    def save():
        args.output.write_text(json.dumps(report, indent=2) + '\n')
    save()
    if args.gpu not in report['devices'] or 'Intel' not in report['devices'][args.gpu]:
        raise RuntimeError(f'Explicit Intel device {args.gpu} unavailable: {report["devices"]}')
    tokenizer = Tokenizer.from_file(str(args.model_dir / 'tokenizer.json'))
    tokenizer.enable_truncation(max_length=args.sequence_length)
    tokenizer.enable_padding(length=args.sequence_length)
    encoded = tokenizer.encode_batch([TEXTS[i % len(TEXTS)] for i in range(args.batch_size)])
    arrays = dict(input_ids=np.array([e.ids for e in encoded], dtype=np.int64),
                  attention_mask=np.array([e.attention_mask for e in encoded], dtype=np.int64),
                  token_type_ids=np.array([e.type_ids for e in encoded], dtype=np.int64))
    model = core.read_model(args.model_dir / 'openvino/openvino_model.xml')
    model.reshape({port.any_name: [args.batch_size, args.sequence_length] for port in model.inputs})
    inputs = {port.any_name: arrays[port.any_name] for port in model.inputs}
    def start_probe(name):
        if not args.connector:
            return None
        env = os.environ.copy()
        env['GSK_RENDERER'] = 'vulkan'
        # Inherit the caller's explicit Vulkan driver selection; never select NVIDIA by accident.
        if not env.get('VK_DRIVER_FILES'):
            raise RuntimeError('Set VK_DRIVER_FILES to the Intel Vulkan manifest for frame probing')
        return subprocess.Popen([args.probe_python, str(Path(__file__).with_name('intel-frame-probe.py')),
                                 '--connector', args.connector, '--seconds', str(args.seconds),
                                 '--output', str(args.output.with_name(name + '-frames.json'))], env=env)
    def finish_probe(probe, name):
        if probe is None:
            return None
        try:
            code = probe.wait(timeout=15)
        except subprocess.TimeoutExpired:
            probe.terminate()
            probe.wait(timeout=5)
            raise RuntimeError('Frame probe failed to exit')
        if code:
            raise RuntimeError(f'Frame probe exited with {code}')
        return json.loads(args.output.with_name(name + '-frames.json').read_text())
    probe = start_probe('baseline')
    time.sleep(args.seconds)
    report['baseline_frames'] = finish_probe(probe, 'baseline')
    save()
    embeddings = {}
    devices = [args.gpu, 'CPU'] if args.gpu_first else ['CPU', args.gpu]
    for device in devices:
        print(f'Compiling {device}', flush=True)
        started = time.perf_counter()
        compiled = core.compile_model(model, device, {'PERFORMANCE_HINT': 'LATENCY',
                                                      'INFERENCE_PRECISION_HINT': 'f32'})
        compile_seconds = time.perf_counter() - started
        execution = list(compiled.get_property('EXECUTION_DEVICES'))
        if device != 'CPU' and (not execution or any(not d.startswith('GPU') for d in execution)):
            raise RuntimeError(f'GPU inference silently routed elsewhere: {execution}')
        request = compiled.create_infer_request()
        def infer():
            request.infer(inputs)
            tokens = request.get_output_tensor(0).data
            mask = arrays['attention_mask'][..., None]
            pooled = (tokens * mask).sum(axis=1) / np.maximum(mask.sum(axis=1), 1)
            return pooled / np.maximum(np.linalg.norm(pooled, axis=1, keepdims=True), 1e-12)
        for _ in range(10):
            embedding = infer()
        if embedding.shape != (args.batch_size, 384) or not np.isfinite(embedding).all():
            raise RuntimeError(f'Unexpected embedding output: {embedding.shape}')
        embeddings[device] = embedding.copy()
        name = device.replace('.', '-')
        probe = start_probe(name)
        latencies = []
        deadline = time.perf_counter() + args.seconds
        while time.perf_counter() < deadline:
            started = time.perf_counter()
            infer()
            latencies.append((time.perf_counter() - started) * 1000)
        frames = finish_probe(probe, name)
        elapsed = sum(latencies) / 1000
        phase = dict(device=device, execution_devices=execution, compile_seconds=compile_seconds,
                     iterations=len(latencies), median_ms=float(np.median(latencies)),
                     p95_ms=float(np.percentile(latencies, 95)), p99_ms=float(np.percentile(latencies, 99)),
                     embeddings_per_second=len(latencies)*args.batch_size/elapsed,
                     embedding_dimensions=int(embedding.shape[-1]), frames=frames)
        report['phases'].append(phase)
        save()
        print(json.dumps(phase), flush=True)
        del request, compiled
    report['cpu_gpu_cosine_similarity'] = [float(v) for v in
                                           (embeddings['CPU'] * embeddings[args.gpu]).sum(axis=1)]
    if min(report['cpu_gpu_cosine_similarity']) < 0.999:
        save()
        raise RuntimeError('CPU/GPU embedding agreement fell below cosine similarity 0.999')
    report['complete'] = True
    save()

if __name__ == '__main__':
    main()
