#!/usr/bin/env python3
"""Print the UDID of an available iPhone simulator.

Used by CI. Kept as a file rather than an inline heredoc because a heredoc inside
a YAML block scalar keeps its indentation, and Python rejects an indented first
line — which failed silently as "empty destination id" instead of a clear error.

    xcrun simctl list devices available -j > sims.json
    python3 scripts/pick_simulator.py sims.json
"""

import json
import sys


def pick(path: str) -> str:
    with open(path, encoding="utf-8") as handle:
        devices = json.load(handle)["devices"]
    # Only iOS runtimes: the list also carries a "My Mac" placeholder, and
    # xcodebuild rejects it as a destination for this scheme.
    for runtime, entries in devices.items():
        if "SimRuntime.iOS-" not in runtime:
            continue
        for entry in entries:
            if entry.get("isAvailable") and entry["name"].startswith("iPhone"):
                return entry["udid"]
    raise SystemExit("no available iPhone simulator in an iOS runtime")


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        print(pick(sys.argv[1]))
    except SystemExit as exc:
        print(exc, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())