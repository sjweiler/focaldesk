#!/usr/bin/env python3
"""Enforce dependency boundaries between FocalDesk's input-domain crates."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


INPUT = "focaldesk-input"
SHORTCUTS = "focaldesk-shortcuts"
ENGINE = "focaldesk-engine"

REQUIRED_DIRECT_RUNTIME = {
    ENGINE: {INPUT, SHORTCUTS},
}
FORBIDDEN_DIRECT_RUNTIME = {
    SHORTCUTS: {"focaldesk-types", "smithay"},
}
REQUIRED_PACKAGES = {INPUT, SHORTCUTS, ENGINE}


def runtime_dependency_graph(metadata: dict) -> dict[str, set[str]]:
    workspace_ids = set(metadata["workspace_members"])
    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package["id"] in workspace_ids
    }
    runtime_direct: dict[str, set[str]] = {name: set() for name in packages}

    for name, package in packages.items():
        for dependency in package["dependencies"]:
            dependency_name = dependency["name"]
            if (
                dependency["kind"] is None
                and not dependency["optional"]
            ):
                runtime_direct[name].add(dependency_name)

    return runtime_direct


def validate(runtime_direct: dict[str, set[str]]) -> list[str]:
    errors = []
    missing_packages = sorted(REQUIRED_PACKAGES - runtime_direct.keys())
    if missing_packages:
        errors.append(f"workspace packages missing: {', '.join(missing_packages)}")
        return errors

    for source, required_dependencies in REQUIRED_DIRECT_RUNTIME.items():
        missing = sorted(required_dependencies - runtime_direct.get(source, set()))
        for dependency in missing:
            errors.append(
                f"missing direct runtime dependency: {source} -> {dependency}"
            )

    for source, forbidden_dependencies in FORBIDDEN_DIRECT_RUNTIME.items():
        present = sorted(forbidden_dependencies & runtime_direct.get(source, set()))
        for dependency in present:
            errors.append(
                f"forbidden direct runtime dependency: {source} -> {dependency}"
            )

    return errors


def load_metadata(repo_root: Path) -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=repo_root,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def self_test() -> None:
    runtime_direct = {
        INPUT: set(),
        SHORTCUTS: {"xkbcommon"},
        ENGINE: {INPUT, SHORTCUTS},
        "focaldesk-types": set(),
    }
    assert validate(runtime_direct) == []

    runtime_direct[ENGINE].remove(SHORTCUTS)
    assert any(
        error == "missing direct runtime dependency: focaldesk-engine -> focaldesk-shortcuts"
        for error in validate(runtime_direct)
    )

    runtime_direct[ENGINE].add(SHORTCUTS)
    runtime_direct[SHORTCUTS].add("smithay")
    assert any(
        error
        == "forbidden direct runtime dependency: focaldesk-shortcuts -> smithay"
        for error in validate(runtime_direct)
    )
    runtime_direct[SHORTCUTS].remove("smithay")

    runtime_direct[SHORTCUTS].add("focaldesk-types")
    assert any(
        error
        == "forbidden direct runtime dependency: focaldesk-shortcuts -> focaldesk-types"
        for error in validate(runtime_direct)
    )
    runtime_direct[SHORTCUTS].remove("focaldesk-types")

    del runtime_direct[INPUT]
    assert any(
        error == "workspace packages missing: focaldesk-input"
        for error in validate(runtime_direct)
    )

    print("Crate-boundary checker self-test passed.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--self-test", action="store_true", help="exercise valid and invalid fixtures"
    )
    args = parser.parse_args()

    if args.self_test:
        self_test()
        return 0

    repo_root = Path(__file__).resolve().parent.parent
    runtime_direct = runtime_dependency_graph(load_metadata(repo_root))
    errors = validate(runtime_direct)
    if errors:
        for error in errors:
            print(f"crate boundary violation: {error}", file=sys.stderr)
        return 1

    print("FocalDesk crate boundaries passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
