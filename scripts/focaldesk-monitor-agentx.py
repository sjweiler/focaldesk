#!/usr/bin/env python3
"""Serve the read-only FocalDesk inventory over a local AgentX socket."""

import argparse
from bisect import bisect_left, bisect_right
from collections import OrderedDict, deque
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
INTEGER, OCTETS, OID, GAUGE, TICKS, COUNTER64 = 2, 4, 6, 66, 67, 70
TRAP_OID = (1, 3, 6, 1, 6, 3, 1, 1, 4, 1, 0)
COMPOSITOR_STALE_MS = 30_000
MAX_GAUGE32 = (1 << 32) - 1
NO_OBJECT, NO_INSTANCE, END_OF_VIEW = 128, 129, 130
STATUS = {"ok": 1, "unsupported": 2, "unavailable": 3, "stale": 4}
GPU_STATE = {"ready": 1, "recovering": 2, "failed": 3, "absent": 4, "unknown": 5, "paused": 6}
DISPLAY_STATE = {
    "active": 1, "disabled": 2, "disconnected": 3, "failed": 4,
    "unknown": 5, "inactive": 6, "notConfigured": 7, "not_configured": 7,
}
INPUT_COUNTERS = {
    8: ("keyboard", "samples"),
    9: ("keyboard", "total_us"),
    10: ("pointer_motion", "samples"),
    11: ("pointer_motion", "total_us"),
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
    19: (COUNTER64, "failures"), 20: (COUNTER64, "recoveries"),
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
    return {root + (1, column) for column in (1, 2, 7, 12, *INPUT_COUNTERS)} | {
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
    age = snapshot.get("sample_age_seconds")
    if age is not None:
        values[root + (1, 12, 0)] = GAUGE, age
    if snapshot.get("sample_status") == "ok":
        input_series = snapshot.get("input_dispatch_delay", {}).get("series", {})
        for column, (kind, field) in INPUT_COUNTERS.items():
            value = input_series.get(kind, {}).get(field)
            if type(value) is int and 0 <= value < 1 << 64:
                values[root + (1, column, 0)] = COUNTER64, value
    for table, rows, columns, states in (
        (2, snapshot["gpus"], GPU_COLUMNS, GPU_STATE),
        (3, snapshot["displays"], DISPLAY_COLUMNS, DISPLAY_STATE),
    ):
        for row in rows:
            for column, (syntax, field) in columns.items():
                value = row.get(field)
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
    if syntax == OID:
        return payload + pack_oid(value)
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


def exchange(peer, kind, session, packet_id, payload):
    send(peer, kind, session, 0, packet_id, payload)
    reply, _, order, response_session, transaction, packet, body = receive(peer)
    _, error, index = struct.unpack_from(order + "IHH", body)
    if (reply != 18 or transaction != 0 or packet != packet_id or error or index
            or (session and response_session != session) or not response_session):
        raise ValueError(f"AgentX request {kind} failed (error {error}, index {index})")
    return response_session


def open_session(peer, description):
    return exchange(peer, 1, 0, 1, bytes((5, 0, 0, 0)) + pack_oid(()) + pack_octets(description))


def register(peer, root):
    session = open_session(peer, b"FocalDesk Vulkan inventory")
    return exchange(peer, 3, session, 2, bytes((5, 127, 0, 0)) + pack_oid(root))


class GpuFailureTracker:
    def __init__(self):
        self.epoch = None
        self.states = {}
        self.recent = {}

    def observe(self, snapshot, now_ms):
        epoch = snapshot["producer_epoch"]
        states = {identity: device["state"] for identity, device in snapshot["gpus"].items()}
        self.recent = {key: until for key, until in self.recent.items() if until > now_ms}
        failed = []
        if epoch == self.epoch:
            for identity, state in states.items():
                if (state == "failed" and self.states.get(identity) not in (None, "failed")
                        and identity not in self.recent):
                    self.recent[identity] = now_ms + 30_000
                    failed.append(identity)
        self.epoch, self.states = epoch, states
        return failed


def gpu_failure_payload(objects_root, notification_oid, epoch, row):
    instance = (row["index"],)
    gpu_entry = objects_root + (2, 1)
    bindings = (
        (TRAP_OID, OID, notification_oid),
        (gpu_entry + (2,) + instance, OCTETS, row["identity"].encode()),
        (gpu_entry + (4,) + instance, INTEGER, GPU_STATE["failed"]),
        (gpu_entry + (6,) + instance, COUNTER64, row["rebuild_attempts"]),
        (gpu_entry + (7,) + instance, COUNTER64, row["rebuild_successes"]),
        (objects_root + (1, 2, 0), OCTETS, epoch.encode()),
    )
    return b"".join(pack_varbind(*binding) for binding in bindings)


class CompositorTracker:
    def __init__(self):
        self.epoch = None
        self.timestamp = None
        self.last_sample_ms = None
        self.stale = False

    def observe(self, fresh, now_ms):
        if fresh is not None:
            epoch, timestamp, age_ms = fresh
            if (epoch, timestamp) != (self.epoch, self.timestamp):
                restarted = self.epoch is not None and epoch != self.epoch
                recovered = self.stale
                self.epoch, self.timestamp = epoch, timestamp
                self.last_sample_ms = now_ms - age_ms
                self.stale = False
                if recovered or restarted:
                    return 6, epoch, age_ms, restarted
        if self.last_sample_ms is not None and not self.stale:
            age_ms = now_ms - self.last_sample_ms
            if age_ms >= COMPOSITOR_STALE_MS:
                self.stale = True
                return 5, self.epoch, age_ms, None
        return None


def compositor_payload(objects_root, suffix, epoch, age_ms, restarted):
    bindings = [
        (TRAP_OID, OID, objects_root[:-1] + (0, suffix)),
        (objects_root + (1, 2, 0), OCTETS, epoch.encode()),
        (objects_root + (1, 12, 0), GAUGE, min(age_ms // 1000, MAX_GAUGE32)),
    ]
    if restarted is not None:
        bindings.append((objects_root + (1, 13, 0), INTEGER, 1 if restarted else 2))
    return b"".join(pack_varbind(*binding) for binding in bindings)


class DisplayTracker:
    def __init__(self):
        self.epoch = None
        self.counts = {}

    def observe(self, snapshot):
        epoch = snapshot["producer_epoch"]
        if epoch != self.epoch:
            self.epoch = epoch
            self.counts.clear()
        events = {}
        for gpu, device in snapshot["gpus"].items():
            for connector, display in device["displays"].items():
                key = (gpu, connector)
                counts = display.get("failures"), display.get("recoveries")
                if None in counts:
                    continue
                previous = self.counts.get(key)
                self.counts[key] = counts
                if previous is not None:
                    changed = tuple(suffix for suffix, before, after in zip((7, 8), previous, counts)
                                    if after > before)
                    if changed:
                        events[key] = changed
        return events


def display_payload(objects_root, suffix, epoch, row):
    index = row["index"]
    entry = objects_root + (3, 1)
    bindings = (
        (TRAP_OID, OID, objects_root[:-1] + (0, suffix)),
        (entry + (2, index), OCTETS, row["identity"].encode()),
        (entry + (5, index), INTEGER, DISPLAY_STATE["failed" if suffix == 7 else "active"]),
        (entry + (19, index), COUNTER64, row["failures"]),
        (entry + (20, index), COUNTER64, row["recoveries"]),
        (objects_root + (1, 2, 0), OCTETS, epoch.encode()),
    )
    return b"".join(pack_varbind(*binding) for binding in bindings)


def watch_notifications(args):
    gpu_tracker = GpuFailureTracker()
    compositor = CompositorTracker()
    displays = DisplayTracker()
    pending = deque(maxlen=32)

    def enqueue(payload):
        if len(pending) == pending.maxlen:
            print("FocalDesk AgentX: notification queue full", file=sys.stderr)
        pending.append((payload, 0))

    while True:
        fresh = None
        monotonic_ms = time.monotonic_ns() // 1_000_000
        try:
            source = INVENTORY["read_snapshot"](args.snapshot)
            now_ms = time.time_ns() // 1_000_000
            age_ms = now_ms - source["sample_unix_ms"]
            if 0 <= age_ms < args.stale_after * 1000:
                fresh = source["producer_epoch"], source["sample_unix_ms"], age_ms
                failed_gpus = (
                    gpu_tracker.observe(source, monotonic_ms) if args.gpu_failure_notification else ()
                )
                display_events = displays.observe(source) if args.display_notifications else {}
                if failed_gpus or display_events:
                    snapshot = INVENTORY["indexed_snapshot"](
                        source, args.registry, args.stale_after, now_ms
                    )
                    epoch = snapshot["producer_epoch"]
                    gpu_ids = {row["index"]: row["identity"] for row in snapshot["gpus"]}
                    for row in snapshot["gpus"]:
                        if row["identity"] in failed_gpus:
                            enqueue(gpu_failure_payload(args.root, args.gpu_failure_notification, epoch, row))
                    for row in snapshot["displays"]:
                        key = (gpu_ids[row["scanout_gpu_index"]], row["connector"])
                        for suffix in display_events.get(key, ()):
                            enqueue(display_payload(args.root, suffix, epoch, row))
        except FileNotFoundError:
            pass
        except (OSError, ValueError, KeyError, TypeError, AttributeError, sqlite3.Error) as exc:
            print(f"FocalDesk AgentX notification snapshot: {exc}", file=sys.stderr, flush=True)
        if args.compositor_notifications:
            event = compositor.observe(fresh, monotonic_ms)
            if event is not None:
                enqueue(compositor_payload(args.root, *event))
        if pending:
            payload, attempts = pending.popleft()
            try:
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
                    peer.settimeout(2)
                    peer.connect(str(args.socket))
                    session = open_session(peer, b"FocalDesk notifications")
                    exchange(peer, 12, session, 2, payload)
            except (OSError, ValueError, struct.error) as exc:
                if attempts < 2:
                    pending.append((payload, attempts + 1))
                else:
                    print(f"FocalDesk AgentX dropped notification: {exc}", file=sys.stderr, flush=True)
        time.sleep(1)


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
    parser.add_argument("--gpu-failure-notification", type=parse_root, metavar="OID",
                        help="enable GPU recovery failure notifications at this numeric OID")
    parser.add_argument("--compositor-notifications", action="store_true",
                        help="send snapshot-stale and recovery notifications under the product root")
    parser.add_argument("--display-notifications", action="store_true",
                        help="send per-connector failure and recovery notifications")
    args = parser.parse_args()
    if args.stale_after < 1:
        parser.error("--stale-after must be positive")
    sensors = NvidiaSampler(args.nvidia_smi) if args.nvidia_smi else None
    if args.gpu_failure_notification or args.compositor_notifications or args.display_notifications:
        Thread(target=watch_notifications, args=(args,), name="agentx-notifications", daemon=True).start()
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
