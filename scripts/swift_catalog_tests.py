"""Compile MirrorCatalog.swift with a local Swift toolchain and run its assertions.

The catalog logic (manifest parsing, mirror rewriting, asset selection, the zip
writer) needs nothing but Foundation, so it can be executed rather than merely
type-checked. That matters: two real bugs in the mirror rules were only found by
running this.

Usage:  python3 scripts/swift_catalog_tests.py
Env:    SWIFTC  — path to swiftc (default: discovered on PATH)
Exit:   0 clean, 1 failures, 2 no toolchain
"""

import os
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CATALOG = ROOT / "app/HorizonPad/Engine/MirrorCatalog.swift"
STORE = ROOT / "app/HorizonPad/Engine/DownloadStore.swift"
WORK = ROOT / ".build/catalog-tests"

MANIFEST = """
{
  "latest": {"release": "26.3", "snapshot": "26.4-snapshot-10"},
  "versions": [
    {"id": "26.3", "type": "release", "releaseTime": "2026-09-15T00:00:00+00:00",
     "url": "https://piston-meta.mojang.com/v1/packages/aa/26.3.json"},
    {"id": "26.4-snapshot-10", "type": "snapshot", "releaseTime": "2026-10-01T00:00:00+00:00",
     "url": "https://piston-meta.mojang.com/v1/packages/bb/26.4.json"},
    {"id": "1.21.4", "type": "release", "releaseTime": "2024-12-03T00:00:00+00:00",
     "url": "https://piston-meta.mojang.com/v1/packages/cc/1.21.4.json"}
  ]
}
"""

VERSION = """
{
  "id": "26.3",
  "downloads": {"client": {
     "sha1": "e877b6a07acd633fb3bb475002175cec036e7b87",
     "size": 41483720,
     "url": "https://piston-data.mojang.com/v1/objects/e877/client.jar"}},
  "assetIndex": {"id": "34", "size": 597235,
     "url": "https://piston-meta.mojang.com/v1/packages/f3df/34.json"}
}
"""

ASSETS = """
{
  "objects": {
    "minecraft/blockstates/stone.json": {"hash": "aaaa1111bbbb2222cccc3333dddd4444eeee5555", "size": 120},
    "minecraft/models/block/stone.json": {"hash": "bbbb1111bbbb2222cccc3333dddd4444eeee5555", "size": 340},
    "minecraft/textures/block/stone.png": {"hash": "cccc1111bbbb2222cccc3333dddd4444eeee5555", "size": 900},
    "minecraft/textures/item/stick.png": {"hash": "dddd1111bbbb2222cccc3333dddd4444eeee5555", "size": 150},
    "minecraft/textures/entity/zombie.png": {"hash": "eeee1111bbbb2222cccc3333dddd4444eeee5555", "size": 5000},
    "minecraft/textures/painting/kz.png": {"hash": "ffff1111bbbb2222cccc3333dddd4444eeee5555", "size": 8000},
    "minecraft/lang/en_us.json": {"hash": "11111111bbbb2222cccc3333dddd4444eeee5555", "size": 900},
    "minecraft/lang/zh_cn.json": {"hash": "22222222bbbb2222cccc3333dddd4444eeee5555", "size": 800},
    "minecraft/lang/de_de.json": {"hash": "33333333bbbb2222cccc3333dddd4444eeee5555", "size": 700},
    "minecraft/sounds.json": {"hash": "44444444bbbb2222cccc3333dddd4444eeee5555", "size": 60000},
    "minecraft/sounds/block/stone/break1.ogg": {"hash": "55555555bbbb2222cccc3333dddd4444eeee5555", "size": 20000},
    "minecraft/shaders/post/blur.json": {"hash": "66666666bbbb2222cccc3333dddd4444eeee5555", "size": 300}
  }
}
"""

TEST_MAIN = r"""
import Foundation

var failures = 0
func check(_ condition: Bool, _ label: String) {
    if condition { print("  ok   " + label) }
    else { print("  FAIL " + label); failures += 1 }
}

let manifestData = Data(MANIFEST_PLACEHOLDER.utf8)
do {
    let manifest = try CatalogDecoder.manifest(from: manifestData)
    check(manifest.latestRelease == "26.3", "manifest: latest release")
    check(manifest.versions.count == 3, "manifest: version count")
    check(manifest.releases.count == 2, "manifest: only releases filtered")
    check(manifest.releases.first?.id == "26.3", "manifest: newest release first")
} catch {
    print("  FAIL manifest decode threw \(error)"); failures += 1
}

do {
    let info = try CatalogDecoder.version(from: Data(VERSION_PLACEHOLDER.utf8))
    check(info.clientJarSize == 41_483_720, "version: client size")
    check(info.assetIndexID == "34", "version: asset index id")
    check(info.clientJarURL.host == "piston-data.mojang.com", "version: client host")
} catch {
    print("  FAIL version decode threw \(error)"); failures += 1
}

let bmcl = DownloadMirror.named("bmclapi")
let official = DownloadMirror.named("official")
let meta = URL(string: "https://piston-meta.mojang.com/v1/packages/f3df/34.json")!
check(bmcl.rewrite(meta).absoluteString == "https://bmclapi2.bangbang93.com/v1/packages/f3df/34.json",
      "mirror: piston-meta rewritten to the mirror root")
check(official.rewrite(meta) == meta, "mirror: official leaves URLs alone")
let objectURL = ObjectURL.forHash("aaaa1111bbbb2222cccc3333dddd4444eeee5555", mirror: bmcl)
check(objectURL.absoluteString
        == "https://bmclapi2.bangbang93.com/assets/aa/aaaa1111bbbb2222cccc3333dddd4444eeee5555",
      "mirror: objects live under /assets")
check(ObjectURL.forHash("aaaa1111bbbb2222cccc3333dddd4444eeee5555", mirror: official).host
        == "resources.download.minecraft.net", "mirror: official object URL untouched")

do {
    let objects = try CatalogDecoder.assetIndex(from: Data(ASSETS_PLACEHOLDER.utf8))
    check(objects.count == 12, "assets: parsed object count")
    let base = AssetPlanner.plan(objects: objects, selection: AssetSelection())
    let paths = Set(base.map(\.path))
    check(paths.contains("assets/minecraft/blockstates/stone.json"), "plan: blockstates kept")
    check(paths.contains("assets/minecraft/models/block/stone.json"), "plan: models kept")
    check(paths.contains("assets/minecraft/textures/block/stone.png"), "plan: block textures kept")
    check(!paths.contains("assets/minecraft/textures/entity/zombie.png"), "plan: entity textures dropped")
    check(!paths.contains("assets/minecraft/textures/painting/kz.png"), "plan: paintings dropped")
    check(paths.contains("assets/minecraft/lang/en_us.json"), "plan: en_us kept")
    check(paths.contains("assets/minecraft/lang/zh_cn.json"), "plan: zh_cn kept")
    check(!paths.contains("assets/minecraft/lang/de_de.json"), "plan: de_de dropped")
    check(paths.contains("assets/minecraft/sounds.json"), "plan: sound catalogue kept")
    check(!paths.contains("assets/minecraft/sounds/block/stone/break1.ogg"), "plan: sounds skipped by default")
    check(!paths.contains("assets/minecraft/shaders/post/blur.json"), "plan: shaders skipped by default")

    var loud = AssetSelection()
    loud.includeSounds = true
    loud.includeShaders = true
    let full = AssetPlanner.plan(objects: objects, selection: loud)
    let fullPaths = Set(full.map(\.path))
    check(fullPaths.contains("assets/minecraft/sounds/block/stone/break1.ogg"), "plan: sounds opt-in")
    check(fullPaths.contains("assets/minecraft/shaders/post/blur.json"), "plan: shaders opt-in")
    check(full.count == base.count + 2, "plan: opt-ins add exactly two entries")
    check(AssetPlanner.totalBytes(base) < AssetPlanner.totalBytes(full), "plan: opt-ins add bytes")
    check(base.map(\.path) == base.map(\.path).sorted(), "plan: deterministic order")
} catch {
    print("  FAIL asset index decode threw \(error)"); failures += 1
}

var writer = StoreZipWriter()
let payloads: [(String, String)] = [
    ("assets/minecraft/blockstates/stone.json", "{\"variants\":{\"\":{\"model\":\"minecraft:block/stone\"}}}"),
    ("assets/minecraft/models/block/stone.json", "{\"textures\":{\"all\":\"minecraft:block/stone\"},\"elements\":[]}"),
    ("assets/minecraft/lang/en_us.json", "{\"block.minecraft.stone\":\"Stone\"}"),
]
for (path, text) in payloads { writer.add(path: path, data: Data(text.utf8)) }
let archive = writer.finalize()
check(writer.count == 3, "zip: entry count")
check(archive.count > 0, "zip: bytes produced")
let outPath = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "catalog-test.zip"
try archive.write(to: URL(fileURLWithPath: outPath))
print("  wrote \(outPath) (\(archive.count) bytes)")
check(CRC32.checksum(Data("123456789".utf8)) == 0xCBF4_3926, "zip: crc32 check value")

// ---- resume planner --------------------------------------------------------
check(ResumePlanner.decide(cachedSize: 0, expectedSize: 100, serverStatus: 200) == .restart,
      "resume: nothing cached restarts")
check(ResumePlanner.decide(cachedSize: 40, expectedSize: 100, serverStatus: 206) == .append(from: 40),
      "resume: partial plus 206 appends from the offset")
check(ResumePlanner.decide(cachedSize: 100, expectedSize: 100, serverStatus: 206) == .done,
      "resume: already complete is done")
check(ResumePlanner.decide(cachedSize: 40, expectedSize: 100, serverStatus: 200) == .restart,
      "resume: a server that ignores the range forces a restart")
check(ResumePlanner.decide(cachedSize: 120, expectedSize: 100, serverStatus: 206) == .restart,
      "resume: a cache larger than expected restarts")
check(ResumePlanner.decide(cachedSize: 40, expectedSize: 0, serverStatus: 206) == .append(from: 40),
      "resume: unknown size still resumes on 206")

// ---- object store ----------------------------------------------------------
do {
    let store = DownloadStore(versionID: "26.3 test/id")
    try store.prepare()
    check(store.hasObject("deadbeef", expectedSize: 4) == false, "store: absent object")
    try store.stashObject(Data("abcd".utf8), hash: "deadbeef")
    check(store.hasObject("deadbeef", expectedSize: 4), "store: stashed object is visible")
    check(store.countObjects() == 1, "store: one object counted")
    check(try store.readObject("deadbeef") == Data("abcd".utf8), "store: bytes round-trip")
    check(store.hasObject("deadbeef", expectedSize: 99) == false,
          "store: size mismatch counts as absent, so a truncated leftover is refetched")
    check(store.byteCount >= 4, "store: byte count covers objects")
    check(DownloadStore.safeName("26.3 test/id") == "26.3_test_id",
          "store: version id sanitised for the filesystem")
    try store.clear()
    check(store.countObjects() == 0, "store: clear empties it")
} catch {
    print("  FAIL store threw \(error)"); failures += 1
}

print(failures == 0 ? "ALL PASSED" : "\(failures) FAILED")
exit(failures == 0 ? 0 : 1)
"""


def find_swiftc() -> pathlib.Path | None:
    env = os.environ.get("SWIFTC")
    if env:
        return pathlib.Path(env)
    found = shutil.which("swiftc")
    if found:
        return pathlib.Path(found)
    # The Windows installer does not put swiftc on PATH by default.
    guess = pathlib.Path.home() / (
        "AppData/Local/Programs/Swift/Toolchains/6.4.3+Asserts/usr/bin/swiftc.exe"
    )
    return guess if guess.exists() else None


def sdk_args(swiftc: pathlib.Path) -> list[str]:
    """Windows needs an explicit SDK; Linux and macOS do not."""
    if sys.platform != "win32":
        return []
    root = swiftc.parents[4]
    candidates = sorted((root / "Platforms").glob("*/Windows.platform/Developer/SDKs/Windows.sdk"))
    return ["-sdk", str(candidates[-1])] if candidates else []


def runtime_path(swiftc: pathlib.Path) -> dict[str, str]:
    env = dict(os.environ)
    if sys.platform == "win32":
        root = swiftc.parents[4]
        runtimes = sorted((root / "Runtimes").glob("*/usr/bin"))
        if runtimes:
            env["PATH"] = str(runtimes[-1]) + os.pathsep + env.get("PATH", "")
    return env


def main() -> int:
    swiftc = find_swiftc()
    if not swiftc or not swiftc.exists():
        print("no Swift toolchain (set SWIFTC or install one); skipping")
        return 2
    if not CATALOG.exists():
        print(f"missing {CATALOG}", file=sys.stderr)
        return 1

    WORK.mkdir(parents=True, exist_ok=True)
    main_swift = WORK / "main.swift"
    main_swift.write_text(
        TEST_MAIN.replace("MANIFEST_PLACEHOLDER", '"' + MANIFEST.replace('"', '\\"').replace("\n", "\\n") + '"')
        .replace("VERSION_PLACEHOLDER", '"' + VERSION.replace('"', '\\"').replace("\n", "\\n") + '"')
        .replace("ASSETS_PLACEHOLDER", '"' + ASSETS.replace('"', '\\"').replace("\n", "\\n") + '"'),
        encoding="utf-8",
    )

    exe = WORK / ("catalog-test.exe" if sys.platform == "win32" else "catalog-test")
    build = subprocess.run(
        [
            str(swiftc),
            *sdk_args(swiftc),
            "-swift-version",
            "5",
            "-O",
            str(CATALOG),
            str(STORE),
            str(main_swift),
            "-o",
            str(exe),
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if build.returncode != 0:
        print(build.stdout)
        print(build.stderr[-4000:])
        return 1

    print(f"swiftc: {swiftc}")
    run = subprocess.run(
        [str(exe), str(WORK / "assets.zip")],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=runtime_path(swiftc),
    )
    print(run.stdout)
    if run.stderr.strip():
        print(run.stderr[-2000:])
    return run.returncode


if __name__ == "__main__":
    sys.exit(main())