//
//  VersionDownloader.swift
//  HorizonPad
//
//  Fetch a game version, resumably.
//
//  The first version started from zero on every attempt: its work directory was
//  UUID-named and deleted on any error, so a dropped connection at 90% of the
//  41 MB client jar meant downloading it again. Users noticed ("总是下载中断就要
//  重新下"), which is the right reaction — that is a design bug, not bad luck.
//
//  What makes progress survive now:
//
//    * one stable directory per version (DownloadStore), never deleted on failure;
//    * asset objects stored by hash, skipped when already present;
//    * the client jar fetched with `Range` and appended to a `.part` file, so an
//      interrupted jar resumes instead of restarting;
//    * every request retried a few times with backoff, mirror then official,
//      without discarding what earlier attempts already fetched.
//

import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
import SwiftUI

@MainActor
final class VersionDownloader: ObservableObject {
    enum Phase: Equatable {
        case idle
        case loadingVersions
        case downloading(step: String, done: Int, total: Int)
        case packing
        case porting
        case done(UUID)
        case failed(String)

        var isBusy: Bool {
            switch self {
            case .idle, .done, .failed: return false
            default: return true
            }
        }
    }

    @Published private(set) var phase: Phase = .idle
    @Published private(set) var versions: [RemoteVersion] = []
    @Published private(set) var manifestLatest: String = ""
    @Published private(set) var cachedObjects: Int = 0
    @Published private(set) var cachedBytes: Int = 0
    @Published var mirrorID: String = "bmclapi"
    @Published var selection = AssetSelection()
    @Published var onlyReleases = true

    private let session: URLSession
    private let fileManager = FileManager.default

    init() {
        let config = URLSessionConfiguration.default
        config.waitsForConnectivity = true
        config.timeoutIntervalForRequest = 60
        config.timeoutIntervalForResource = 3600
        config.httpAdditionalHeaders = ["User-Agent": "HorizonPad"]
        session = URLSession(configuration: config)
    }

    var mirror: DownloadMirror { DownloadMirror.named(mirrorID) }

    var visibleVersions: [RemoteVersion] {
        onlyReleases ? versions.filter(\.isRelease) : versions
    }

    // MARK: - Version list

    func loadVersions() async {
        guard !phase.isBusy else { return }
        phase = .loadingVersions
        do {
            let data = try await get(mirror.manifestURL)
            let manifest = try CatalogDecoder.manifest(from: data)
            versions = manifest.versions
            manifestLatest = manifest.latestRelease
            phase = .idle
        } catch {
            phase = .failed(describe(error))
        }
    }

    /// What is already on disk for `version`, so the UI can say "续传" honestly.
    func refreshCacheInfo(for version: RemoteVersion) {
        let store = DownloadStore(versionID: version.id)
        cachedObjects = store.countObjects()
        cachedBytes = store.byteCount
    }

    func clearCache(for version: RemoteVersion) {
        try? DownloadStore(versionID: version.id).clear()
        cachedObjects = 0
        cachedBytes = 0
    }

    // MARK: - One download

    @discardableResult
    func download(_ version: RemoteVersion) async -> UUID? {
        guard !phase.isBusy else { return nil }
        let store = DownloadStore(versionID: version.id)
        do {
            try store.prepare()

            // 1. version metadata (small, not cached: it changes rarely but a
            //    stale copy would silently pin an old asset index)
            phase = .downloading(step: "版本信息", done: 0, total: 1)
            let metaData = try await getWithFallbackData(version.metadataURL)
            let info = try CatalogDecoder.version(from: metaData)

            // 2. asset index
            phase = .downloading(step: "资源索引", done: 0, total: 1)
            let indexData = try await getWithFallbackData(info.assetIndexURL)
            let objects = try CatalogDecoder.assetIndex(from: indexData)
            let plan = AssetPlanner.plan(objects: objects, selection: selection)
            guard !plan.isEmpty else {
                throw CatalogError.badJSON("asset selection produced no files")
            }

            // 3. client jar, resumable: this is the file people noticed
            phase = .downloading(step: "client.jar（可续传）", done: 0, total: plan.count + 1)
            try await fetchResumable(
                upstream: info.clientJarURL,
                destination: store.objectURL("client.jar"),
                expectedSize: info.clientJarSize
            )

            // 4. asset objects, skipping what earlier attempts already stored
            let missing = plan.filter { !store.hasObject($0.hash, expectedSize: $0.size) }
            cachedObjects = plan.count - missing.count
            if !missing.isEmpty {
                try await fetchObjects(missing, store: store, totalAssets: plan.count)
            }

            // 5. pack
            phase = .packing
            var writer = StoreZipWriter()
            for asset in plan {
                guard let bytes = try? store.readObject(asset.hash) else { continue }
                writer.add(path: asset.path, data: bytes)
            }
            if let jar = try? store.readObject("client.jar") {
                // The client jar is not an asset; it rides along for hz-class.
                writer.add(path: "client.jar", data: jar)
            }
            try writer.finalize().write(to: store.packURL, options: .atomic)

            // 6. port
            phase = .porting
            let id = UUID()
            let packURL = Paths.packURL(for: id)
            let reportURL = Paths.reportURL(for: id)
            let outcome: PortOutcome = try await withCheckedThrowingContinuation { continuation in
                Engine.queue.async {
                    do {
                        let result = try Porter.port(
                            jar: store.packURL,
                            pack: packURL,
                            report: reportURL,
                            minecraftVersion: version.id,
                            progress: { _ in }
                        )
                        continuation.resume(returning: result)
                    } catch {
                        continuation.resume(throwing: error)
                    }
                }
            }

            // 7. register
            let report = ImportController.parseReport(outcome.reportJSON)
            let pack = InstalledPack(
                id: id,
                displayName: "Minecraft \(version.id)",
                minecraftVersion: version.id,
                engineAPI: Int(Engine.apiVersion),
                packBytes: outcome.pack.bytes,
                entryCount: outcome.pack.entries,
                kinds: outcome.pack.kinds,
                manifestSHA256: outcome.pack.manifestSHA256,
                coverage: report.coverage,
                javaClasses: report.javaClasses,
                jvmSummary: report.jvm
            )
            Library.shared.add(pack)
            store.removeIntermediate()
            phase = .done(id)
            return id
        } catch {
            // Deliberately keep the store: it *is* the progress.
            let kept = store.countObjects()
            let suffix = kept > 0 ? "（已保留 \(kept) 个文件，重新开始会续传）" : ""
            phase = .failed(describe(error) + suffix)
            return nil
        }
    }

    // MARK: - Networking

    private func fetchObjects(_ plan: [SelectedAsset], store: DownloadStore, totalAssets: Int) async throws {
        var completed = totalAssets - plan.count
        let limit = 6

        try await withThrowingTaskGroup(of: Void.self) { group in
            var iterator = plan.makeIterator()
            var inFlight = 0
            var firstError: Error?

            func addNext() {
                guard let asset = iterator.next() else { return }
                inFlight += 1
                group.addTask { [weak self] in
                    guard let self else { return }
                    let data = try await self.getObject(asset)
                    try store.stashObject(data, hash: asset.hash)
                }
            }

            for _ in 0..<limit { addNext() }

            while inFlight > 0 {
                do {
                    try await group.next()
                } catch {
                    // One bad object must not throw away the others' progress;
                    // remember it and keep draining the group.
                    if firstError == nil { firstError = error }
                }
                inFlight -= 1
                completed += 1
                phase = .downloading(step: "资源", done: completed, total: totalAssets)
                addNext()
            }

            if let firstError { throw firstError }
        }
    }

    /// Fetch one object, verifying its hash. Retries are handled by
    /// `getWithFallback`; a hash mismatch is treated as a corrupt transfer and
    /// retried once more.
    private func getObject(_ asset: SelectedAsset) async throws -> Data {
        for attempt in 1...3 {
            let data = try await getWithFallbackData(
                ObjectURL.forHash(asset.hash, mirror: mirror)
            )
            if InsecureSHA1.hex(data) == asset.hash {
                return data
            }
            if attempt == 3 {
                throw CatalogError.badJSON("资源校验失败 \(asset.objectPath)")
            }
            try await Task.sleep(nanoseconds: UInt64(attempt) * 400_000_000)
        }
        throw CatalogError.badJSON("资源校验失败 \(asset.objectPath)")
    }

    /// Download with `Range` resume.
    ///
    /// The `.part` file is appended to in place: an interrupted transfer leaves a
    /// prefix on disk and the next attempt asks only for what is missing.
    ///
    /// A complete `.part` is *moved into place*, not reported as done: "the bytes
    /// are on disk" and "the file exists at its destination" are different facts,
    /// and conflating them left a finished download sitting in a `.part` file.
    private func fetchResumable(upstream: URL, destination: URL, expectedSize: Int) async throws {
        let part = destination.appendingPathExtension("part")

        func size(of url: URL) -> Int {
            (try? fileManager.attributesOfItem(atPath: url.path)[.size] as? Int) ?? 0
        }

        func finalize() throws {
            if fileManager.fileExists(atPath: destination.path) {
                try? fileManager.removeItem(at: destination)
            }
            try fileManager.moveItem(at: part, to: destination)
        }

        if expectedSize > 0, size(of: destination) == expectedSize { return }
        if expectedSize > 0, size(of: part) == expectedSize {
            try finalize()
            return
        }

        let candidates = candidateURLs(upstream)
        var lastError: Error?

        for url in candidates {
            var offset = size(of: part)
            for attempt in 1...4 {
                if expectedSize > 0, offset == expectedSize {
                    try finalize()
                    return
                }
                var request = URLRequest(url: url)
                if offset > 0 {
                    request.setValue("bytes=\(offset)-", forHTTPHeaderField: "Range")
                }
                do {
                    let (bytes, response) = try await session.data(for: request)
                    let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                    switch ResumePlanner.decide(
                        cachedSize: offset,
                        expectedSize: expectedSize,
                        serverStatus: status
                    ) {
                    case .done:
                        try finalize()
                        return
                    case .restart:
                        try? fileManager.removeItem(at: part)
                        try bytes.write(to: part, options: .atomic)
                    case .append:
                        let handle = try FileHandle(forWritingTo: part)
                        try handle.seekToEnd()
                        try handle.write(contentsOf: bytes)
                        try handle.close()
                    }

                    let written = size(of: part)
                    if expectedSize > 0, written == expectedSize {
                        try finalize()
                        return
                    }
                    if expectedSize == 0 { break }
                    offset = written
                    if attempt == 4 {
                        throw CatalogError.badJSON(
                            "jar 下载不完整（\(written)/\(expectedSize)），下次会续传"
                        )
                    }
                    try await Task.sleep(nanoseconds: UInt64(attempt) * 500_000_000)
                } catch {
                    lastError = error
                    if attempt == 4 { break }
                    try await Task.sleep(nanoseconds: UInt64(attempt) * 700_000_000)
                    offset = size(of: part)
                }
            }
        }
        throw lastError ?? URLError(.cannotLoadFromNetwork)
    }

    private func candidateURLs(_ upstream: URL) -> [URL] {
        let mirrored = mirror.rewrite(upstream)
        return mirrored == upstream ? [upstream] : [mirrored, upstream]
    }

    private func get(_ url: URL) async throws -> Data {
        var lastError: Error?
        for attempt in 1...3 {
            do {
                let (data, response) = try await session.data(for: URLRequest(url: url))
                if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                    throw URLError(.badServerResponse)
                }
                return data
            } catch {
                lastError = error
                try await Task.sleep(nanoseconds: UInt64(attempt) * 400_000_000)
            }
        }
        throw lastError ?? URLError(.cannotLoadFromNetwork)
    }

    private func getWithFallbackData(_ upstream: URL) async throws -> Data {
        var lastError: Error?
        for candidate in candidateURLs(upstream) {
            do {
                return try await get(candidate)
            } catch {
                lastError = error
            }
        }
        throw lastError ?? URLError(.cannotLoadFromNetwork)
    }

    private func describe(_ error: Error) -> String {
        if let catalog = error as? CatalogError {
            return catalog.errorDescription ?? "\(catalog)"
        }
        if let url = error as? URLError {
            return "网络错误：\(url.localizedDescription)"
        }
        return error.localizedDescription
    }
}

/// SHA-1 without pulling in CryptoKit's interface differences across SDKs.
enum InsecureSHA1 {
    static func hex(_ data: Data) -> String {
        var h: [UInt32] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0]
        var message = [UInt8](data)
        let bitLength = UInt64(message.count) * 8
        message.append(0x80)
        while message.count % 64 != 56 { message.append(0) }
        for shift in stride(from: 56, through: 0, by: -8) {
            message.append(UInt8((bitLength >> UInt64(shift)) & 0xFF))
        }
        var w = [UInt32](repeating: 0, count: 80)
        var offset = 0
        while offset < message.count {
            for i in 0..<16 {
                let base = offset + i * 4
                w[i] = (UInt32(message[base]) << 24)
                    | (UInt32(message[base + 1]) << 16)
                    | (UInt32(message[base + 2]) << 8)
                    | UInt32(message[base + 3])
            }
            for i in 16..<80 {
                w[i] = rotl(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1)
            }
            var a = h[0], b = h[1], c = h[2], d = h[3], e = h[4]
            for i in 0..<80 {
                let (f, k): (UInt32, UInt32)
                switch i {
                case 0..<20: f = (b & c) | (~b & d); k = 0x5A827999
                case 20..<40: f = b ^ c ^ d; k = 0x6ED9EBA1
                case 40..<60: f = (b & c) | (b & d) | (c & d); k = 0x8F1BBCDC
                default: f = b ^ c ^ d; k = 0xCA62C1D6
                }
                let temp = rotl(a, 5) &+ f &+ e &+ k &+ w[i]
                e = d
                d = c
                c = rotl(b, 30)
                b = a
                a = temp
            }
            h[0] = h[0] &+ a
            h[1] = h[1] &+ b
            h[2] = h[2] &+ c
            h[3] = h[3] &+ d
            h[4] = h[4] &+ e
            offset += 64
        }
        return h.map { String(format: "%08x", $0) }.joined()
    }

    private static func rotl(_ value: UInt32, _ count: UInt32) -> UInt32 {
        (value << count) | (value >> (32 - count))
    }
}