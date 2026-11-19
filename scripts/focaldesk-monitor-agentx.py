#!/usr/bin/env python3
"""Serve the read-only FocalDesk inventory over a local AgentX socket."""

import argparse
from bisect import bisect_left, bisect_right
from collections import OrderedDict
import csv
from pathlib import Path
import re
import runpy
import socket
import sqlite3
import struct
import subprocess
import sys
from threading import Thread
import time


INVENTORY = runpy.run_path(str(Path(__file__).with_name("focaldesk-monitor-inventory.py")))
INTEGER, OCTETS, GAUGE, TICKS, COUNTER64 = 2, 4, 66, 67, 70
NO_OBJECT, NO_INSTANCE, END_OF_VIEW = 128, 129, 130
STATUS = {"ok": 1, "unsupported": 2, "unavailable": 3, "stale": 4}
GPU_STATE = {"ready": 1, "recovering": 2, "failed": 3, "absent": 4, "unknown": 5, "paused": 6}
DISPLAY_STATE = {
    "active": 1, "disabled": 2, "disconnected": 3, "failed": 4,
    "unknown": 5, "inactive": 6, "notConfigured": 7, "not_configured": 7,
}
GPU_COLUMNS = {
    2: (OCTETS, "identity"), 4: (INTEGER, "state"),
    5: (COUNTER64, "device_losses"), 6: (COUNTER64, "rebuild_attempts"),
    7: (COUNTER64, "rebuild_successes"), 13: (INTEGER, "sample_status"),
    14: (GAUGE, "sample_age_seconds"),
}
SENSOR_COLUMNS = {15: 100, 16: 0xffffffff, 17: 0xffffffff}
PCI_ADDRESS = re.compile(r"([0-9a-f]{4,8}):([0-9a-f]{2}:[0-9a-f]{2}\.[0-7])", re.I)
DISPLAY_COLUMNS = {
    2: (OCTETS, "identity"), 4: (INTEGER, "scanout_gpu_index"), 5: (INTEGER, "state"),
    6: (GAUGE, "width"), 7: (GAUGE, "height"), 8: (GAUGE, "refresh_millihz"),
    10: (COUNTER64, "presented_frames"), 17: (INTEGER, "sample_status"),
    18: (GAUGE, "sample_age_seconds"),
}


def pci_address(value):
    match = PCI_ADDRESS.fullmatch(value)
    if match:
        return f"{int(match[1], 16):04x}:{match[2].lower()}"
    return None


def nvidia_sensors(output):
    devices = {}
    for row in csv.reader(output.splitlines(), skipinitialspace=True):
        if len(row) != 4:
            continue
        address = pci_address(row[0].strip())
        if address is None:
            continue
        values = {}
        for (column, maximum), raw in zip(SENSOR_COLUMNS.items(), row[1:]):
            try:
                value = int(raw)
            except ValueError:
                continue
            if 0 <= value <= maximum:
                values[column] = value
        devices[address] = values
    return devices


class NvidiaSampler:
    def __init__(self, command):
        self.sample = (0.0, {})
        Thread(target=self.poll, args=(command,), name="nvidia-sensors", daemon=True).start()

    def poll(self, command):
        while True:
            try:
                result = subprocess.run(
                    [command, "--query-gpu=pci.bus_id,utilization.gpu,memory.used,memory.total",
                     "--format=csv,noheader,nounits"],
                    capture_output=True, text=True, errors="replace", timeout=2, check=True,
                )
                self.sample = (time.monotonic(), nvidia_sensors(result.stdout))
            except (OSError, subprocess.SubprocessError):
                # Preserve one recent sample through a transient driver error.
                pass
            time.sleep(5)

    def current(self):
        timestamp, values = self.sample
        return values if time.monotonic() - timestamp < 15 else {}


def leaf_prefixes(root):
    return {root + (1, column) for column in (1, 2, 7)} | {
        root + (table, 1, column)
        for table, columns in ((2, GPU_COLUMNS.keys() | SENSOR_COLUMNS.keys()), (3, DISPLAY_COLUMNS))
        for column in columns
    }


def objects(root, snapshot, sensors=None):
    values = {
        root + (1, 1, 0): (TICKS, (snapshot["uptime_seconds"] * 100) % (1 << 32)),
        root + (1, 2, 0): (OCTETS, snapshot["producer_epoch"].encode()),
        root + (1, 7, 0): (OCTETS, bytes.fromhex(snapshot["inventory_epoch"])),
    }
    for table, rows, columns, states in (
        (2, snapshot["gpus"], GPU_COLUMNS, GPU_STATE),
        (3, snapshot["displays"], DISPLAY_COLUMNS, DISPLAY_STATE),
    ):
        for row in rows:
            for column, (syntax, field) in columns.items():
                value = row[field]
                if value is None:
                    continue
                if field == "state":
                    value = states[value]
                elif field == "sample_status":
                    value = STATUS[value]
                elif syntax == OCTETS:
                    value = value.encode()
                values[root + (table, 1, column, row["index"])] = syntax, value
    if sensors:
        for row in snapshot["gpus"]:
            if row["state"] == "absent" or row["sample_status"] != "ok":
                continue
            address = pci_address(row["identity"].rsplit("/", 1)[-1])
            for column, value in sensors.get(address, {}).items():
                values[root + (2, 1, column, row["index"])] = GAUGE, value
    return values


def pack_oid(oid):
    return bytes((len(oid), 0, 0, 0)) + struct.pack(f"!{len(oid)}I", *oid)


def pack_octets(value):
    return struct.pack("!I", len(value)) + value + bytes(-len(value) % 4)


def read_oid(payload, offset, order):
    count, prefix, include, _ = struct.unpack_from("4B", payload, offset)
    if include not in (0, 1) or count + (5 if prefix else 0) > 128:
        raise ValueError("invalid AgentX OID")
    oid = struct.unpack_from(f"{order}{count}I", payload, offset + 4)
    if prefix:
        oid = (1, 3, 6, 1, prefix) + oid
    return oid, bool(include), offset + 4 + 4 * count


def pack_varbind(oid, syntax, value):
    payload = struct.pack("!HH", syntax, 0) + pack_oid(oid)
    if syntax == OCTETS:
        return payload + pack_octets(value)
    if syntax == COUNTER64:
        return payload + struct.pack("!Q", value)
    if syntax in (INTEGER, GAUGE, TICKS):
        return payload + struct.pack("!I", value)
    return payload


def query(kind, payload, order, values, prefixes):
    offset = 0
    ranges = []
    if kind == 7:
        nonrepeaters, repetitions = struct.unpack_from(order + "HH", payload)
        offset = 4
    while offset < len(payload):
        start, include, offset = read_oid(payload, offset, order)
        end, _, offset = read_oid(payload, offset, order)
        ranges.append((start, include, end))
    ordered = sorted(values)

    def next_binding(start, include, end):
        index = (bisect_left if include else bisect_right)(ordered, start)
        if index < len(ordered) and (not end or ordered[index] < end):
            oid = ordered[index]
            return oid, *values[oid]
        return start, END_OF_VIEW, None

    bindings = []
    if kind == 5:
        for oid, _, _ in ranges:
            missing = NO_INSTANCE if any(oid[:len(p)] == p for p in prefixes) else NO_OBJECT
            bindings.append((oid, *values.get(oid, (missing, None))))
    elif kind == 6:
        bindings = [next_binding(*search_range) for search_range in ranges]
    else:
        bindings = [next_binding(*search_range) for search_range in ranges[:nonrepeaters]]
        repeaters = ranges[nonrepeaters:]
        if len(bindings) + len(repeaters) * repetitions > 4096:
            return 1, b""  # tooBig
        for _ in range(repetitions if repeaters else 0):
            row = [next_binding(*search_range) for search_range in repeaters]
            bindings.extend(row)
            if all(syntax == END_OF_VIEW for _, syntax, _ in row):
                break
            repeaters = [(oid, False, end) for (oid, _, _), (_, _, end) in zip(row, repeaters)]
    return 0, b"".join(pack_varbind(*binding) for binding in bindings)


def receive_exact(peer, size):
    result = bytearray()
    while len(result) < size:
        chunk = peer.recv(size - len(result))
        if not chunk:
            raise ConnectionError("AgentX master disconnected")
        result.extend(chunk)
    return result


def receive(peer):
    header = receive_exact(peer, 20)
    version, kind, flags, reserved = header[:4]
    order = "!" if flags & 16 else "<"
    session, transaction, packet, size = struct.unpack_from(order + "4I", header, 4)
    if version != 1 or reserved or size > 1_048_576 or size % 4:
        raise ValueError("invalid AgentX header")
    return kind, flags, order, session, transaction, packet, receive_exact(peer, size)


def send(peer, kind, session, transaction, packet, payload):
    peer.sendall(struct.pack("!4B4I", 1, kind, 16, 0, session, transaction, packet, len(payload)) + payload)


def register(peer, root):
    exchanges = (
        (1, 0, bytes((5, 0, 0, 0)) + pack_oid(()) + pack_octets(b"FocalDesk Vulkan inventory")),
        (3, None, bytes((5, 127, 0, 0)) + pack_oid(root)),
    )
    session = 0
    for packet_id, (kind, session_override, payload) in enumerate(exchanges, start=1):
        send(peer, kind, session if session_override is None else session_override, 0, packet_id, payload)
        reply, _, order, response_session, transaction, packet, body = receive(peer)
        _, error, index = struct.unpack_from(order + "IHH", body)
        if reply != 18 or transaction != 0 or packet != packet_id or error or index:
            raise ValueError(f"AgentX registration failed (error {error})")
        session = response_session
    return session


def serve(peer, args, sensors=None):
    session = register(peer, args.root)
    prefixes = leaf_prefixes(args.root)
    transactions = OrderedDict()
    while True:
        kind, flags, order, request_session, transaction, packet, payload = receive(peer)
        if request_session != session:
            raise ValueError("AgentX session mismatch")
        if kind == 11:  # CleanupSet has no response.
            continue
        error, result = 0, b""
        if flags & 8:
            error = 262  # unsupportedContext
        elif kind in (5, 6, 7):
            now = time.monotonic()
            while transactions and next(iter(transactions.values()))[0] <= now:
                transactions.popitem(last=False)
            if transaction not in transactions:
                try:
                    source = INVENTORY["read_snapshot"](args.snapshot)
                    snapshot = INVENTORY["indexed_snapshot"](
                        source, args.registry, args.stale_after, time.time_ns() // 1_000_000
                    )
                    values = objects(args.root, snapshot, sensors.current() if sensors else None)
                except (OSError, ValueError, KeyError, TypeError, AttributeError, sqlite3.Error) as exc:
                    print(f"FocalDesk AgentX inventory: {exc}", file=sys.stderr, flush=True)
                    values = {}
                transactions[transaction] = now + 10, values
                if len(transactions) > 16:
                    transactions.popitem(last=False)
            _, values = transactions[transaction]
            try:
                error, result = query(kind, payload, order, values, prefixes)
            except (ValueError, struct.error):
                error = 266  # parseError
        elif kind in (8, 9, 10):
            error = 17  # notWritable
        elif kind not in (2, 13):
            error = 267  # requestDenied
        send(peer, 18, session, transaction, packet, struct.pack("!IHH", 0, error, int(error == 17)) + result)
        if kind == 2:  # Close
            return


def parse_root(value):
    try:
        oid = tuple(int(part) for part in value.removeprefix(".").split("."))
    except ValueError as exc:
        raise argparse.ArgumentTypeError("expected a numeric OID") from exc
    if not 5 <= len(oid) <= 120 or oid[:4] != (1, 3, 6, 1) or any(
        part < 0 or part > 0xffffffff for part in oid
    ):
        raise argparse.ArgumentTypeError("expected a bounded numeric OID below 1.3.6.1")
    if oid[:7] == (1, 3, 6, 1, 4, 1, 32473):
        raise argparse.ArgumentTypeError("the documentation PEN is reserved for offline examples")
    return oid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True, type=Path, help="private Unix AgentX socket")
    parser.add_argument("--root", required=True, type=parse_root, help="authorized fdObjects OID")
    parser.add_argument("--snapshot", required=True, type=Path)
    parser.add_argument("--registry", required=True, type=Path)
    parser.add_argument("--stale-after", type=int, default=5)
    parser.add_argument("--nvidia-smi", metavar="PATH", help="enable background NVIDIA GPU sensors")
    args = parser.parse_args()
    if args.stale_after < 1:
        parser.error("--stale-after must be positive")
    sensors = NvidiaSampler(args.nvidia_smi) if args.nvidia_smi else None
    while True:
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
                peer.connect(str(args.socket))
                serve(peer, args, sensors)
        except (OSError, ValueError, struct.error) as exc:
            print(f"FocalDesk AgentX: {exc}", file=sys.stderr, flush=True)
        time.sleep(1)


if __name__ == "__main__":
    main()
