#!/usr/bin/env python3
"""Render an importable OpenNMS collection source for a FocalDesk agent."""

import argparse
from pathlib import Path
import runpy


def main():
    directory = Path(__file__).resolve().parent
    adapter = directory.parents[1] / "scripts/focaldesk-monitor-agentx.py"
    parse_oid = runpy.run_path(str(adapter))["parse_root"]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("objects_oid", type=parse_oid, help="numeric fdObjects OID")
    parser.add_argument("--sys-object-id", type=parse_oid, help="defaults to the objects OID")
    args = parser.parse_args()

    def dotted(oid):
        return "." + ".".join(map(str, oid))

    template = (directory / "focaldesk.xml.in").read_text()
    print(template.replace("@FD_OBJECTS@", dotted(args.objects_oid))
          .replace("@SYS_OBJECT_ID@", dotted(args.sys_object_id or args.objects_oid)),
          end="")


if __name__ == "__main__":
    main()
