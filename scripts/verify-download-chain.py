"""Prove the download chain without a network.

Three claims, each checked rather than assumed:

  1. the pure catalog logic passes its assertions (scripts/swift_catalog_tests.py);
  2. the zip that logic writes is a container the Rust engine accepts;
  3. that container ports into a .hzp.

Steps 2 and 3 are the ones that matter: they show the *bytes* a download would
produce are bytes the engine can consume, not merely that unit tests are green.

Usage:  python3 scripts/verify-download-chain.py
"""

import json
import pathlib
import shutil
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORK = ROOT / ".build/catalog-tests"

if sys.platform == "win32":
    HZ = ROOT / "engine/target/release/hz.exe"
else:
    HZ = ROOT / "engine/target/release/hz"


def run(cmd, **kwargs):
    return subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", **kwargs)


def hz(*args: str):
    return run([str(HZ), *args])


def main() -> int:
    if not HZ.exists():
        print(f"build the CLI first: cargo build --release -p hz-cli (missing {HZ})", file=sys.stderr)
        return 1

    print("== 1. catalog logic ==")
    result = run([sys.executable, str(ROOT / "scripts/swift_catalog_tests.py")])
    output = result.stdout + result.stderr
    print(output[-2500:])
    if result.returncode == 2:
        print("no Swift toolchain; the remaining stages still run")
    elif result.returncode != 0:
        return 1

    zip_path = WORK / "assets.zip"
    if not zip_path.exists():
        print("the tests produced no zip; nothing to verify", file=sys.stderr)
        return 1

    print("== 2. the zip is a container the engine accepts ==")
    unpacked = WORK / "unpacked"
    if unpacked.exists():
        shutil.rmtree(unpacked)
    out = hz("unzip", str(zip_path), str(unpacked))
    print(out.stdout.strip() or out.stderr.strip())
    if "0 failures" not in (out.stdout + out.stderr):
        print("the engine rejected the Swift-written zip", file=sys.stderr)
        return 1
    files = sorted(p.relative_to(unpacked).as_posix() for p in unpacked.rglob("*") if p.is_file())
    for name in files:
        print("   ", name)

    print("== 3. it ports ==")
    hzp = WORK / "assets.hzp"
    report = WORK / "assets.report.json"
    out = hz("port", str(zip_path), str(hzp), "--quiet", "--version", "26.3", "--report", str(report))
    print(out.stdout.strip() or out.stderr.strip())
    if out.returncode != 0 or not hzp.exists():
        print("port failed", file=sys.stderr)
        return 1

    data = json.loads(report.read_text(encoding="utf-8"))
    print("coverage:", data.get("coverage"), "kinds:", data.get("counts"))
    print("\nall stages agree: the catalog's output is a pack the engine can build.")
    return 0


if __name__ == "__main__":
    sys.exit(main())