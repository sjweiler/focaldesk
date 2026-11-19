"""Exercise the FocalDesk AgentX adapter through an anonymous socket pair."""
import json
from pathlib import Path
import runpy
import socket
import struct
import tempfile
import threading
import time
from types import SimpleNamespace


def main():
    agent = runpy.run_path(str(
        Path(__file__).resolve().parents[2] / "scripts/focaldesk-monitor-agentx.py"
    ))
    # The synthetic tree is confined to this anonymous socket pair.
    root = (1, 3, 6, 1, 3, 42)
    verify_sensor_mapping(agent, root)
    verify_input_timing(agent, root)
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory)
        snapshot = path / "snapshot.json"
        sample = {
            "schema_version": 1, "producer_epoch": "test", "uptime_seconds": 42,
            "sample_unix_ms": time.time_ns() // 1_000_000 - 10_000,
            "gpus": {"gpu-a": {
                "state": "ready", "device_losses": 1 << 40, "rebuild_attempts": 1,
                "rebuild_successes": 1, "displays": {"DP-1": {
                    "state": "active", "width": None, "height": None,
                    "refresh_millihz": None, "presented_frames": 1 << 48,
                }},
            }},
        }
        args = SimpleNamespace(root=root, snapshot=snapshot, registry=path / "indexes",
                               stale_after=5)
        for session in (55, 56):
            sample["uptime_seconds"] = 42
            snapshot.write_text(json.dumps(sample))
            verify_session(agent, args, session, sample)
    print("Verified registration, Counter64, sparse values, stale status, GETBULK, "
          "transaction consistency, denied SET, a second session, GPU sensors, and input timing.")

def verify_sensor_mapping(agent, root):
    sensors = agent["nvidia_sensors"](
        "00000000:01:00.0, 0, 10240, 24564\n00000000:02:00.0, N/A, N/A, 50000"
    )
    snapshot = {
        "uptime_seconds": 1, "producer_epoch": "test", "inventory_epoch": "00" * 16,
        "gpus": [], "displays": [],
    }
    row = {
        "index": 1, "identity": "/sys/devices/pci0000:00/0000:01:00.0",
        "state": "ready", "device_losses": 0, "rebuild_attempts": 0, "rebuild_successes": 0,
        "sample_status": "ok", "sample_age_seconds": 0,
    }
    snapshot["gpus"] = [row, dict(row, index=2, identity="/sys/devices/0000:02:00.0")]
    values = agent["objects"](root, snapshot, sensors)
    assert values[root + (2, 1, 15, 1)] == (66, 0)
    assert values[root + (2, 1, 16, 1)] == (66, 10240)
    assert values[root + (2, 1, 17, 1)] == (66, 24564)
    assert root + (2, 1, 15, 2) not in values
    assert values[root + (2, 1, 17, 2)] == (66, 50000)
    row["sample_status"] = "stale"
    values = agent["objects"](root, snapshot, sensors)
    assert root + (2, 1, 15, 1) not in values


def verify_input_timing(agent, root):
    snapshot = {
        "uptime_seconds": 1, "producer_epoch": "test", "inventory_epoch": "00" * 16,
        "sample_status": "ok", "gpus": [], "displays": [],
    }
    key_count = root + (1, 8, 0)
    key_total = root + (1, 9, 0)
    pointer_count = root + (1, 10, 0)
    pointer_total = root + (1, 11, 0)
    assert key_count not in agent["objects"](root, snapshot)
    snapshot["input_dispatch_delay"] = {"series": {
        "keyboard": {"samples": 7, "total_us": 1 << 40},
        "pointer_motion": {"samples": 0, "total_us": 0},
    }}
    values = agent["objects"](root, snapshot)
    assert values[key_count] == (70, 7)
    assert values[key_total] == (70, 1 << 40)
    assert values[pointer_count] == values[pointer_total] == (70, 0)
    snapshot["sample_status"] = "stale"
    assert key_count not in agent["objects"](root, snapshot)


def verify_session(agent, args, session, sample):
    master, peer = socket.socketpair()
    master.settimeout(3)

    def worker():
        with peer:
            agent["serve"](peer, args)

    thread = threading.Thread(target=worker)
    thread.start()
    with master:
        for kind in (1, 3):
            frame = agent["receive"](master)
            assert frame[0] == kind
            agent["send"](master, 18, session, 0, frame[5], bytes(8))

        def call(kind, transaction, body=b"", endian="!"):
            flags = 16 if endian == "!" else 0
            header = struct.pack(endian + "4B4I", 1, kind, flags, 0,
                                 session, transaction, 100, len(body))
            master.sendall(header + body)
            frame = agent["receive"](master)
            assert frame[:6] == (18, 16, "!", session, transaction, 100)
            return frame[6]

        def search_range(start, endian="!"):
            return bytes((len(start), 0, 0, 0)) + struct.pack(
                endian + f"{len(start)}I", *start
            ) + bytes(4)

        root = args.root
        loss = root + (2, 1, 5, 1)
        sparse = root + (3, 1, 6, 1)
        stale = root + (2, 1, 13, 1)
        uptime = root + (1, 1, 0)
        pack = agent["pack_varbind"]
        assert call(5, 1, search_range(loss, "<"), "<")[8:] == pack(loss, 70, 1 << 40)
        assert call(5, 2, search_range(sparse))[8:] == pack(sparse, 129, None)
        assert call(5, 3, search_range(stale))[8:] == pack(stale, 2, 4)
        bulk = call(7, 4, struct.pack("!HH", 0, 2) + search_range(root))
        assert bulk[8:].startswith(pack(uptime, 67, 4200))
        sample["uptime_seconds"] = 43
        args.snapshot.write_text(json.dumps(sample))
        assert call(6, 4, search_range(root))[8:] == pack(uptime, 67, 4200)
        assert struct.unpack("!IHH", call(8, 5)) == (0, 17, 1)
        call(2, 6, bytes(4))
    thread.join(3)
    assert not thread.is_alive()


if __name__ == "__main__":
    main()
