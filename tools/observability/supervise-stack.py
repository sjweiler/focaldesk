#!/usr/bin/env python3
"""Run Compose under a user service; notify systemd only after local readiness."""
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import time
import urllib.request

ENDPOINTS = ('http://127.0.0.1:13133/', 'http://127.0.0.1:9090/-/ready',
             'http://127.0.0.1:3000/api/health', 'http://127.0.0.1:16686/api/services')


def ready(endpoints=ENDPOINTS):
    for url in endpoints:
        try:
            with urllib.request.urlopen(url, timeout=2) as response:
                if response.status != 200:
                    return False
        except (OSError, ValueError):
            return False
    return True


def notify(message):
    address = os.environ.get('NOTIFY_SOCKET')
    if address:
        if address.startswith('@'):
            address = '\0' + address[1:]
        with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as client:
            client.connect(address)
            client.sendall(message.encode())


def main():
    compose_file = Path(__file__).resolve().with_name('compose.yaml')
    docker = shutil.which('docker')
    if not docker:
        raise SystemExit('docker compose (or the Podman docker compatibility command) is required')
    command = [docker, 'compose', '-f', str(compose_file)]
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    process = subprocess.Popen(command + ['up', '--no-color', '--abort-on-container-exit'])
    deadline = time.monotonic() + 150
    announced = False
    misses = 0
    next_check = 0.0
    try:
        while not stopping:
            if process.poll() is not None:
                print('Compose exited unexpectedly; systemd will restart the stack', flush=True)
                return 1
            now = time.monotonic()
            if now >= next_check:
                healthy = ready()
                next_check = time.monotonic() + 5
                if healthy:
                    misses = 0
                    if not announced:
                        notify('READY=1\nSTATUS=Collector, Prometheus, Grafana and Jaeger are ready')
                        announced = True
                elif not announced and now >= deadline:
                    print('Stack did not become ready before the startup deadline', flush=True)
                    return 1
                elif announced:
                    misses += 1
                    if misses >= 3:
                        print('Stack readiness failed three consecutive checks; restarting', flush=True)
                        return 1
            time.sleep(0.25)
        return 0
    finally:
        # Stop containers, preserving named volumes and existing traces when
        # no stop/restart is needed. Never remove persistent volumes.
        try:
            subprocess.run(command + ['stop', '-t', '20'], timeout=40, check=False)
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == '__main__':
    raise SystemExit(main())
