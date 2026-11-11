#!/usr/bin/env python3
"""Activate local tracing builds using reversible user service drop-ins."""
from pathlib import Path
import argparse
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--apply', action='store_true', help='write overrides and restart services')
args = parser.parse_args()
repo = Path(__file__).resolve().parents[2]
vector = repo.parent / 'focal-vector'
services = {
    'focal-vector.service': (vector / 'target/release/focal-server', 'FOCAL_VECTOR_OTEL'),
    'focaldesk-server.service': (repo / 'target/debug/focaldesk-server', 'FOCALDESK_OTEL'),
    'focaldesk-ai-console-app.service': (repo / 'target/debug/focaldesk-ai-console', 'FOCALDESK_OTEL'),
}
base = Path.home() / '.config/systemd/user'
stack_unit = f'''[Unit]
Description=FocalDesk local observability stack
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=notify
NotifyAccess=main
WorkingDirectory={repo}/tools/observability
ExecStart=/usr/bin/python3 "{repo}/tools/observability/supervise-stack.py"
Restart=on-failure
RestartSec=10
TimeoutStartSec=180
TimeoutStopSec=60
KillMode=mixed

[Install]
WantedBy=default.target
'''
print(f'{base / "focaldesk-observability.service"}\n{stack_unit}')
# Check every executable before writing any configuration.
if args.apply:
    for binary, _ in services.values():
        if not binary.is_file():
            raise SystemExit(f'Missing built executable: {binary}')
    base.mkdir(parents=True, exist_ok=True)
    (base / 'focaldesk-observability.service').write_text(stack_unit)
for unit, (binary, flag) in services.items():
    if args.apply and not binary.is_file():
        raise SystemExit(f'Missing built executable: {binary}')
    content = (f'[Unit]\nWants=focaldesk-observability.service\nAfter=focaldesk-observability.service\n\n[Service]\nExecStart=\nExecStart="{binary}"\n'
               f'Environment={flag}=1\n'
               'Environment=OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318\n'
               'Environment=OTEL_METRIC_EXPORT_INTERVAL=5000\n')
    if unit == 'focaldesk-server.service':
        content += 'Environment=FOCALDESK_OLLAMA_KEEP_ALIVE=20m\nEnvironment=FOCALDESK_OLLAMA_PREWARM=1\n'
    if unit == 'focal-vector.service':
        # The packaged sidecar permits only Unix sockets; OTLP needs loopback TCP.
        content += 'RestrictAddressFamilies=\nRestrictAddressFamilies=AF_UNIX AF_INET AF_INET6\n'
    path = base / f'{unit}.d/90-local-observability.conf'
    print(f'{path}\n{content}')
    if args.apply:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
if args.apply:
    subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
    subprocess.run(['systemctl', '--user', 'enable', 'focaldesk-observability.service'], check=True)
    # A first Compose handoff can fail while old containers are being replaced;
    # permit the unit's configured restart to recover before failing activation.
    subprocess.run(['systemctl', '--user', 'start', 'focaldesk-observability.service'], check=False)
    deadline = time.monotonic() + 45
    while subprocess.run(['systemctl', '--user', 'is-active', '--quiet', 'focaldesk-observability.service']).returncode:
        if time.monotonic() >= deadline:
            raise SystemExit('Observability did not become ready; inspect its user-service journal')
        time.sleep(1)
    subprocess.run(['systemctl', '--user', 'restart', *services], check=True)
