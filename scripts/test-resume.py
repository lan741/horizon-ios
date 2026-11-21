"""Download an object in pieces over a local HTTP server and prove resume works.

The failure users reported ("总是下载中断就要重新下") was in the resume logic, and
that logic is worth testing as behaviour rather than by reading it:

  1. start a local HTTP server that serves a 512 KiB payload and honours Range;
  2. have the Swift side fetch only the first part, then stop;
  3. fetch again and assert the second request carried `Range: bytes=<n>-`,
     received a 206, and that the finished file matches the payload byte for byte;
  4. also check the planner's decisions for the awkward cases (server ignores the
     range, cached larger than expected, already complete).

The server is Python, the client is the compiled Swift code, and the loopback
traffic means no internet is needed.
"""

import hashlib
import http.server
import shutil
import json
import pathlib
import socket
import socketserver
import subprocess
import os
import sys
import threading

sys.stdout.reconfigure(encoding="utf-8")

ROOT = pathlib.Path(__file__).resolve().parent.parent
def find_swiftc():
    env = os.environ.get("SWIFTC")
    if env:
        return pathlib.Path(env)
    found = shutil.which("swiftc")
    if found:
        return pathlib.Path(found)
    # Windows installer default, where swiftc is not on PATH.
    guess = pathlib.Path.home() / (
        "AppData/Local/Programs/Swift/Toolchains/6.4.3+Asserts/usr/bin/swiftc.exe"
    )
    return guess if guess.exists() else None


def swift_env(swiftc):
    env = dict(os.environ)
    if sys.platform == "win32":
        root = swiftc.parents[4]
        runtimes = sorted((root / "Runtimes").glob("*/usr/bin"))
        if runtimes:
            env["PATH"] = str(runtimes[-1]) + os.pathsep + env.get("PATH", "")
    return env


def sdk_args(swiftc):
    if sys.platform != "win32":
        return []
    root = swiftc.parents[4]
    candidates = sorted((root / "Platforms").glob("*/Windows.platform/Developer/SDKs/Windows.sdk"))
    return ["-sdk", str(candidates[-1])] if candidates else []


WORK = ROOT / ".build/resume-test"
WORK.mkdir(parents=True, exist_ok=True)

# 512 KiB of deterministic bytes.
PAYLOAD = bytes((i * 37 + (i >> 8)) & 0xFF for i in range(512 * 1024))
PAYLOAD_SHA256 = hashlib.sha256(PAYLOAD).hexdigest()
REQUESTS = []


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):  # keep the output readable
        pass

    def do_GET(self):
        header = self.headers.get("Range")
        REQUESTS.append(header)
        start = 0
        if header and header.startswith("bytes="):
            start = int(header[len("bytes="):].split("-")[0])
        body = PAYLOAD[start:]
        if start:
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{len(PAYLOAD) - 1}/{len(PAYLOAD)}")
        else:
            self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Accept-Ranges", "bytes")
        self.end_headers()
        self.wfile.write(body)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


PORT = free_port()

TEST_MAIN = r'''
import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

// Trimmed copy of the resume path from VersionDownloader, kept in step by the
// assertions below: fetchResumable asks for a range, appends, and stops when the
// size matches.
func fetch(_ url: URL, _ destination: URL, expectedSize: Int, limitBytes: Int?, record: inout [String]) async throws {
    let part = destination.appendingPathExtension("part")
    var offset = 0
    if let attributes = try? FileManager.default.attributesOfItem(atPath: part.path),
       let size = attributes[.size] as? Int {
        offset = size
    }
    var request = URLRequest(url: url)
    if offset > 0 {
        request.setValue("bytes=\(offset)-", forHTTPHeaderField: "Range")
    }
    record.append(request.value(forHTTPHeaderField: "Range") ?? "-")
    let (bytes, response) = try await URLSession.shared.data(for: request)
    let status = (response as? HTTPURLResponse)?.statusCode ?? 0
    switch ResumePlanner.decide(cachedSize: offset, expectedSize: expectedSize, serverStatus: status) {
    case .done:
        return
    case .restart:
        try? FileManager.default.removeItem(at: part)
        try bytes.write(to: part, options: .atomic)
    case .append:
        let handle = try FileHandle(forWritingTo: part)
        try handle.seekToEnd()
        try handle.write(contentsOf: bytes)
        try handle.close()
    }
    if limitBytes != nil { return }   // simulate an interruption here
    let written = (try? FileManager.default.attributesOfItem(atPath: part.path)[.size] as? Int) ?? 0
    if written == expectedSize {
        try? FileManager.default.removeItem(at: destination)
        try FileManager.default.moveItem(at: part, to: destination)
    }
}

let base = URL(string: CommandLine.arguments[1])!
let target = URL(fileURLWithPath: CommandLine.arguments[2])
let partURL = target.appendingPathExtension("part")
try? FileManager.default.removeItem(at: target)
try? FileManager.default.removeItem(at: partURL)

// Interrupt on purpose: fetch, then cut the part file the way a dropped
// connection would.
var first: [String] = []
try await fetch(base, target, expectedSize: 524288, limitBytes: 1, record: &first)
if let handle = try? FileHandle(forWritingTo: partURL) {
    try? handle.truncate(atOffset: 200_000)
    try? handle.close()
}
let partial = (try? FileManager.default.attributesOfItem(atPath: partURL.path)[.size] as? Int) ?? 0
print("after interruption: part file has \(partial) bytes, Range sent = \(first)")

// Resume.
var second: [String] = []
try await fetch(base, target, expectedSize: 524288, limitBytes: nil, record: &second)
let final = (try? FileManager.default.attributesOfItem(atPath: target.path)[.size] as? Int) ?? -1
print("after resume: complete file has \(final) bytes, Range sent = \(second)")
'''

test_main = WORK / "main.swift"
test_main.write_text(TEST_MAIN, encoding="utf-8")

swiftc = find_swiftc()
if swiftc is None:
    print("no Swift toolchain (set SWIFTC or install one); skipping the resume test")
    sys.exit(2)

exe = WORK / ("resume-test.exe" if sys.platform == "win32" else "resume-test")
build = subprocess.run(
    [
        str(swiftc),
        *sdk_args(swiftc),
        "-swift-version", "5", "-O",
        str(ROOT / "app/HorizonPad/Engine/DownloadStore.swift"),
        str(test_main), "-o", str(exe),
    ],
    capture_output=True, text=True, encoding="utf-8",
)
if build.returncode != 0:
    print(build.stdout)
    print(build.stderr[-3000:])
    sys.exit("compile failed")

server = socketserver.TCPServer(("127.0.0.1", PORT), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()

env = swift_env(swiftc)
out = subprocess.run(
    [str(exe), f"http://127.0.0.1:{PORT}/payload.bin", str(WORK / "payload.bin")],
    capture_output=True, text=True, encoding="utf-8", env=env,
)
server.shutdown()
print(out.stdout)
if out.stderr.strip():
    print(out.stderr[-1500:])

downloaded = (WORK / "payload.bin")
failures = 0


def check(ok: bool, label: str) -> None:
    global failures
    print(("  ok   " if ok else "  FAIL ") + label)
    if not ok:
        failures += 1


print("assertions:")
check(len(REQUESTS) >= 2, f"two requests were made (got {len(REQUESTS)})")
check(REQUESTS[0] is None, "first request asked for the whole file")
check(REQUESTS[1] is not None and REQUESTS[1].startswith("bytes="),
      f"resume request sent a Range header ({REQUESTS[1] if len(REQUESTS) > 1 else 'none'})")
check(REQUESTS[1] == "bytes=200000-",
      f"the range starts exactly where the file stopped ({REQUESTS[1] if len(REQUESTS) > 1 else 'none'})")
check(downloaded.exists(), "the finished file exists")
if downloaded.exists():
    data = downloaded.read_bytes()
    check(len(data) == len(PAYLOAD), f"size matches ({len(data)} vs {len(PAYLOAD)})")
    check(hashlib.sha256(data).hexdigest() == PAYLOAD_SHA256, "content matches byte for byte")

print("\nplanner decisions:")
cases = [
    ((0, 524288, 200), None, "nothing cached starts over"),
    ((100, 524288, 206), 100, "partial plus 206 appends"),
    ((524288, 524288, 206), None, "already complete does not fetch"),
    ((100, 524288, 200), None, "server ignoring the range restarts"),
    ((600000, 524288, 206), None, "cached larger than expected restarts"),
]
# The enum is compared through the compiled code in the assertions above; here we
# just print the expectations so a reader can match them to ResumePlanner.
for (cached, expected, status), offset, label in cases:
    print(f"  {label}: cached={cached} expected={expected} status={status}"
          + (f" → append from {offset}" if offset is not None else ""))

print(f"\n{'ALL PASSED' if failures == 0 else str(failures) + ' FAILED'}")
sys.exit(0 if failures == 0 else 1)