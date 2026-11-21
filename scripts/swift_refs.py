# A narrow "did I reference something that does not exist" check for Swift.
#
# The host cannot compile SwiftUI/Metal code, and a patch once introduced a
# `sessionLock` that only the macOS build would have caught. This script looks
# for exactly that shape: a camelCase identifier called as `name(...)` that no
# file in the app declares and that is not an SDK name.
#
# Deliberately conservative — comments and string literals are stripped first,
# declarations are collected app-wide, and only call syntax (`name(`) is
# considered, because member access on locals is where the false positives live.
# A noisy check gets switched off, so a handful of real hits is the target, not
# full coverage.

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP = ROOT / "app"

SDK = {
    "print", "min", "max", "abs", "zip", "stride", "task", "uuid", "data",
    "date", "url", "locale", "string", "int", "double", "float", "bool",
    "array", "dictionary", "set", "hypot", "cos", "sin", "tan", "logger",
    "timer", "nsobject", "nserror", "error", "xctestcase", "uttype", "bundle",
    "processinfo", "userdefaults", "filemanager", "bytecountformatter",
    "iso8601dateformatter", "dateformatter", "utsname", "uname",
    "mtlcreatesystemdefaultdevice", "mtkview", "mtlregionmake2d",
    "cacurrentmediatime", "simdlength", "simdnormalize", "simdcross",
    "simddot", "simdfloat4x4", "precondition", "assert", "fatalfailure",
    "fatalerror", "unsafebitcast", "withunsafebytes",
    "withunsafemutablebufferpointer", "withextendedlifetime", "clamp",
}

# Names that come from outside the app by design: the C ABI (`hz_*`, checked
# against the header by swift_check.py) and the simd module.
EXTERNAL_PREFIXES = ("hz_", "simd_")

KEYWORDS = {
    "if", "guard", "switch", "return", "for", "while", "repeat", "defer",
    "do", "try", "throw", "catch", "case", "else", "in", "where", "func",
    "var", "let", "class", "struct", "enum", "protocol", "extension", "init",
    "self", "super", "import", "typealias", "static", "public", "private",
    "internal", "fileprivate", "override", "mutating", "subscript", "get",
    "set", "willSet", "didSet", "async", "await", "closure",
}


def strip(src):
    """Remove comments and string literals so prose cannot look like code."""
    out = []
    i = 0
    n = len(src)
    while i < n:
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
        if src[i] == '"':
            # triple-quoted
            if src.startswith('"""', i):
                j = src.find('"""', i + 3)
                i = n if j < 0 else j + 3
                continue
            i += 1
            while i < n:
                if src[i] == "\\":
                    i += 2
                    continue
                if src[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        out.append(src[i])
        i += 1
    return "".join(out)


DECL = re.compile(
    r"\b(?:func|var|let|class|struct|enum|protocol|extension|case|typealias)\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)"
)
CALL = re.compile(r"(?<![\w.@])([a-z][A-Za-z0-9_]*)\s*\(")


def main():
    if not APP.is_dir():
        print("no app directory at %s" % APP, file=sys.stderr)
        return 1
    files = sorted(APP.rglob("*.swift"))
    declared = set()
    stripped = {}
    for f in files:
        code = strip(f.read_text(encoding="utf-8"))
        stripped[f] = code
        declared |= set(DECL.findall(code))
        # labels and parameter names used as calls elsewhere are common; treat
        # every `name:` label as known so argument labels do not trip the check
        declared |= set(re.findall(r"([A-Za-z_][A-Za-z0-9_]*)\s*:", code))

    problems = []
    for f, code in stripped.items():
        for m in CALL.finditer(code):
            name = m.group(1)
            if name in declared or name in SDK or name in KEYWORDS:
                continue
            if name.startswith(EXTERNAL_PREFIXES):
                continue
            if name.lower() in SDK:
                continue
            line = code[: m.start()].count("\n") + 1
            problems.append(f"{f.name}:{line}: `{name}(` is called but never declared in the app")

    for p in problems:
        print(p)
    print(f"checked {len(files)} Swift files: {'FAIL' if problems else 'ok'}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())