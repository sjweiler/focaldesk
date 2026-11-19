#!/usr/bin/env python3
"""Read a Vulkan snapshot and assign persistent SNMP-compatible row indexes."""

import argparse
import json
import os
from pathlib import Path
import sqlite3
import time
import uuid


MAX_INDEX = (1 << 31) - 1
MAX_GAUGE = (1 << 32) - 1


def read_snapshot(path):
    snapshot = json.loads(path.read_text())
    if snapshot["schema_version"] != 1:
        raise ValueError("unsupported Vulkan snapshot schema")
    if type(snapshot["sample_unix_ms"]) is not int or snapshot["sample_unix_ms"] < 0:
        raise ValueError("invalid sample timestamp")
    if not isinstance(snapshot["producer_epoch"], str) or not snapshot["producer_epoch"]:
        raise ValueError("missing producer epoch")
    return snapshot


def inventory(database, identities_by_table):
    database.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(database, os.O_CREAT | os.O_RDWR, 0o600)
    os.close(fd)
    connection = sqlite3.connect(database)
    try:
        with connection:
            connection.execute("BEGIN IMMEDIATE")
            connection.execute("""
                CREATE TABLE IF NOT EXISTS inventory_epoch (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    epoch TEXT NOT NULL
                )
            """)
            connection.execute(
                "INSERT OR IGNORE INTO inventory_epoch VALUES (1, ?)",
                (uuid.uuid4().hex,),
            )
            epoch, = connection.execute("SELECT epoch FROM inventory_epoch").fetchone()
            indexes = {}
            for kind, identities in identities_by_table.items():
                # Table names come from the caller's fixed GPU/display mapping.
                connection.execute(f"""
                    CREATE TABLE IF NOT EXISTS {kind} (
                        row_index INTEGER PRIMARY KEY CHECK (row_index BETWEEN 1 AND {MAX_INDEX}),
                        identity TEXT NOT NULL UNIQUE
                    )
                """)
                rows = {}
                for identity in sorted(identities):
                    connection.execute(
                        f"INSERT INTO {kind} (identity) SELECT ? "
                        f"WHERE NOT EXISTS (SELECT 1 FROM {kind} WHERE identity = ?)",
                        (identity, identity),
                    )
                    rows[identity], = connection.execute(
                        f"SELECT row_index FROM {kind} WHERE identity = ?", (identity,)
                    ).fetchone()
                indexes[kind] = rows
        return epoch, indexes
    finally:
        connection.close()


def indexed_snapshot(snapshot, database, stale_after_seconds, now_ms):
    gpu_data = snapshot["gpus"]
    displays = {
        json.dumps([gpu, connector], separators=(",", ":")): (gpu, connector, display)
        for gpu, device in gpu_data.items()
        for connector, display in device["displays"].items()
    }
    epoch, indexes = inventory(database, {"gpu": gpu_data, "display": displays})
    elapsed_ms = now_ms - snapshot["sample_unix_ms"]
    if elapsed_ms < 0:
        status, age = "unavailable", None
    else:
        status = "stale" if elapsed_ms >= stale_after_seconds * 1000 else "ok"
        age = min(elapsed_ms // 1000, MAX_GAUGE)
    freshness = {"sample_status": status, "sample_age_seconds": age}
    return {
        **{key: value for key, value in snapshot.items() if key != "gpus"},
        "inventory_epoch": epoch,
        **freshness,
        "gpus": [
            {
                **{key: value for key, value in device.items() if key != "displays"},
                "index": indexes["gpu"][identity],
                "identity": identity,
                **freshness,
            }
            for identity, device in gpu_data.items()
        ],
        "displays": [
            {
                **display,
                "index": indexes["display"][identity],
                "identity": identity,
                "connector": connector,
                "scanout_gpu_index": indexes["gpu"][gpu],
                **freshness,
            }
            for identity, (gpu, connector, display) in displays.items()
        ],
    }


def main():
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    default_snapshot = Path(runtime) / "focaldesk-vulkan-metrics.json" if runtime else None
    state = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local/state")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--snapshot", type=Path, default=default_snapshot)
    parser.add_argument("--registry", type=Path, default=state / "focaldesk/monitor-inventory.sqlite3")
    parser.add_argument("--stale-after", type=int, default=5, metavar="SECONDS")
    args = parser.parse_args()
    if args.snapshot is None:
        parser.error("provide --snapshot or set XDG_RUNTIME_DIR")
    if args.stale_after < 1:
        parser.error("--stale-after must be positive")
    try:
        snapshot = read_snapshot(args.snapshot)
        print(json.dumps(indexed_snapshot(
            snapshot, args.registry, args.stale_after, time.time_ns() // 1_000_000
        ), indent=2))
    except (OSError, ValueError, KeyError, TypeError, AttributeError, sqlite3.Error) as error:
        parser.exit(1, f"FocalDesk inventory: {error}\n")


if __name__ == "__main__":
    main()
