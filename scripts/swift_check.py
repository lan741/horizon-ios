"""Structural sanity check for the Swift sources.

We cannot run swiftc on this machine, so this script does the cheap checks that
catch the mistakes a human actually makes in hand-written Swift:

  * balanced (), [], {} after removing comments and string literals, with Swift
    string interpolation (which nests) handled properly
  * every `final class`/`struct`/`enum` that opens a brace also closes it
  * `#if`/`#endif` parity
  * no tabs-at-line-start mixed with spaces in the same file
  * a few API pairs that must appear in lockstep (C header ↔ Swift wrapper)

Exit code 1 on any finding.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# Located relative to this file so the same script works on a developer machine
# and on a CI runner.
ROOT = Path(__file__).resolve().parent.parent
APP = ROOT / "app"
ENGINE = ROOT / "engine"


def strip_swift(src: str) -> str:
    """Remove comments and string literal *contents*, keeping structure."""
    out: list[str] = []
    i = 0
    n = len(src)
    # stack of contexts: "code" | "string" | "interp"
    stack: list[str] = ["code"]
    while i < n:
        c = src[i]
        ctx = stack[-1]
        if ctx == "code":
            if src.startswith("//", i):
                j = src.find("\n", i)
                i = n if j < 0 else j
                continue
            if src.startswith("/*", i):
                depth = 1
                i += 2
                while i < n and depth:
                    if src.startswith("/*", i):
                        depth += 1
                        i += 2
                    elif src.startswith("*/", i):
                        depth -= 1
                        i += 2
                    else:
                        i += 1
                continue
            if c == '"':
                # raw string?
                if src.startswith('"""', i):
                    i += 3
                    while i < n and not src.startswith('"""', i):
                        i += 1
                    i += 3
                    continue
                stack.append("string")
                i += 1
                continue
            out.append(c)
            if c in "([{":
                out.append("")
            i += 1
            continue
        if ctx == "string":
            if c == "\\" and i + 1 < n:
                nxt = src[i + 1]
                if nxt == "(":
                    stack.append("interp")
                    out.append("()")  # placeholder keeps parens balanced
                    i += 2
                    continue
                i += 2
                continue
            if c == '"':
                stack.pop()
                i += 1
                continue
            i += 1
            continue
        # interp: code inside \( ... ), ends at the matching )
        if c == "(":
            stack.append("paren")
            i += 1
            continue
        if c == ")":
            stack.pop()
            i += 1
            continue
        if c == '"':
            stack.append("string")
            i += 1
            continue
        i += 1
        continue
    return "".join(out)


def check_file(path: Path) -> list[str]:
    problems: list[str] = []
    src = path.read_text(encoding="utf-8")
    code = strip_swift(src)

    for opener, closer in [("(", ")"), ("[", "]"), ("{", "}")]:
        a, b = code.count(opener), code.count(closer)
        if a != b:
            problems.append(f"{path.name}: {a} × {opener} vs {b} × {closer}")

    if src.count("#if") != src.count("#endif"):
        problems.append(f"{path.name}: #if/#endif mismatch")

    for lineno, line in enumerate(src.splitlines(), 1):
        if "\t" in line and line.startswith(" "):
            problems.append(f"{path.name}:{lineno}: mixed indent")

    # A `func` whose body never opens is almost always a copy/paste error.
    for m in re.finditer(r"\bfunc\s+[A-Za-z_][A-Za-z0-9_]*\s*(?:<[^>]*>)?\s*\(", code):
        tail = code[m.start(): m.start() + 400]
        if "{" not in tail and "->" not in tail:
            problems.append(f"{path.name}: func near offset {m.start()} has no body")

    return problems


def check_lockstep() -> list[str]:
    """Every C symbol the header declares must be called somewhere in the app."""
    header = (ENGINE / "include" / "hz_ffi.h").read_text(encoding="utf-8")
    swift = "\n".join(f.read_text(encoding="utf-8") for f in sorted(APP.rglob("*.swift")))
    symbols = set(re.findall(r"\bhz_[a-z_]+\s*\(", header))
    problems = []
    for symbol in sorted(symbols):
        name = symbol.rstrip("( ").strip()
        if name not in swift:
            problems.append(f"no Swift file calls {name} (declared in hz_ffi.h)")
    return problems


def run_reference_check() -> list[str]:
    """Second opinion from scripts/swift_refs.py (undeclared call targets)."""
    import subprocess
    import sys

    script = Path(__file__).with_name("swift_refs.py")
    if not script.exists():
        return []
    proc = subprocess.run([sys.executable, str(script)], capture_output=True, text=True)
    if proc.returncode == 0:
        return []
    return [line for line in proc.stdout.splitlines() if line.strip() and "checked" not in line]


def main() -> int:
    if not APP.is_dir():
        print(f"no app directory at {APP}", file=sys.stderr)
        return 1
    files = sorted(APP.rglob("*.swift"))
    if not files:
        print("no Swift files found", file=sys.stderr)
        return 1
    problems: list[str] = []
    for f in files:
        problems.extend(check_file(f))
    problems.extend(check_lockstep())
    problems.extend(run_reference_check())

    for p in problems:
        print(p)
    print(f"checked {len(files)} Swift files: {'FAIL' if problems else 'ok'}")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())