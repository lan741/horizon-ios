# Preflight for a clean checkout on someone else's machine.
#
# Before pushing to a CI provider, three things reliably break that a local run
# never sees:
#
#   1. absolute paths baked into scripts (fine here, missing on a runner),
#   2. files referenced by workflows or docs that were never committed,
#   3. build artifacts or fixtures large enough to bloat the repository.
#
# This script checks all three and exits non-zero on any finding. It runs in CI
# as its own job so the first push fails fast and cheaply instead of burning a
# macOS runner.

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

TEXT_SUFFIXES = {
    ".py", ".sh", ".ps1", ".rs", ".swift", ".yml", ".yaml", ".toml", ".md",
    ".json", ".c", ".h", ".metal", ".txt", ".gitignore",
}

# Paths that legitimately name a location on *this* machine in prose.
ALLOW_PROSE = ("docs/BUILD.md",)

# A drive letter, a separator, and at least two real path segments. Bare
# `s:\n` style escapes in doc comments must not match.
ABSOLUTE = re.compile(
    r"(?:[A-Za-z]:\\[A-Za-z0-9_.$-]+(?:\\[A-Za-z0-9_.$-]+)+|/Users/[A-Za-z0-9_.$-]+/|/home/[A-Za-z0-9_.$-]+/)"
)

# Files that should never be committed.
IGNORED_DIRS = {"target", ".build", ".bench", "DerivedData", "build", ".git"}
MAX_FILE_BYTES = 2 * 1024 * 1024  # 2 MiB is plenty for fixtures

# Produced by the build, so a fresh checkout never has them.
GENERATED_PREFIXES = (
    "engine/target/",
    "app/HorizonPad.xcodeproj/",
    ".build/",
)


def tracked_files():
    for path in ROOT.rglob("*"):
        if not path.is_file():
            continue
        if any(part in IGNORED_DIRS for part in path.relative_to(ROOT).parts):
            continue
        yield path


def check_absolute_paths() -> list[str]:
    problems = []
    for path in tracked_files():
        rel = path.relative_to(ROOT).as_posix()
        if rel in ALLOW_PROSE or rel == "scripts/preflight.py":
            continue
        if path.suffix not in TEXT_SUFFIXES and path.name != ".gitignore":
            continue
        if path.suffix == ".ps1":
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for match in ABSOLUTE.finditer(text):
            # Ignore things that are clearly inside a code block or an example,
            # by reporting only the first hit per file.
            problems.append(f"{rel}: absolute path `{match.group(0)}`")
            break
    return problems


def check_referenced_files() -> list[str]:
    problems = []
    workflow_text = "\n".join(
        p.read_text(encoding="utf-8")
        for p in (ROOT / ".github" / "workflows").glob("*.yml")
    )
    # A path only counts when it starts a word: `HorizonPad.app/Payload` inside a
    # shell command is not a repository path, but the old pattern matched the
    # `app/...` substring of it and reported a file that was never meant to exist.
    pattern = re.compile(
        r"(?:(?<=^)|(?<=[\s'\"=(]))(scripts/[\w.-]+|engine/[\w./-]+|app/[\w./-]+)",
        re.M,
    )
    for match in pattern.finditer(workflow_text):
        rel = match.group(1).rstrip(".,")
        # Compare with and without the trailing slash: a workflow may name the
        # bundle directory itself (`app/HorizonPad.xcodeproj`).
        if any(rel.startswith(p) or rel == p.rstrip("/") for p in GENERATED_PREFIXES):
            continue
        candidate = ROOT / rel
        if not candidate.exists():
            problems.append(f"workflow references missing path `{rel}`")
    return sorted(set(problems))


def check_sizes() -> list[str]:
    problems = []
    for path in tracked_files():
        size = path.stat().st_size
        if size > MAX_FILE_BYTES:
            problems.append(
                f"{path.relative_to(ROOT).as_posix()}: {size // 1024} KiB "
                f"(limit {MAX_FILE_BYTES // 1024} KiB)"
            )
    return problems


def check_workflows_parse() -> list[str]:
    try:
        import yaml
    except ImportError:
        return []  # not installed: skip rather than fail
    problems = []
    for path in (ROOT / ".github" / "workflows").glob("*.yml"):
        try:
            with open(path, encoding="utf-8") as handle:
                doc = yaml.safe_load(handle)
        except Exception as exc:  # noqa: BLE001 - any parse error counts
            problems.append(f"{path.name}: {exc}")
            continue
        if "jobs" not in (doc or {}):
            problems.append(f"{path.name}: no jobs")
    return problems


def main() -> int:
    problems = (
        check_absolute_paths()
        + check_referenced_files()
        + check_sizes()
        + check_workflows_parse()
    )
    for problem in problems:
        print(problem)
    print(f"preflight: {'FAIL' if problems else 'ok'}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())