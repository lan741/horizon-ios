//
//  VersionDownloader.swift
//  HorizonPad
//
//  The in-app download path: pick a version, fetch it through a mirror, pack the
//  result, port it, register it. Networking only — every decision about *what* to
//  fetch lives in `MirrorCatalog`, which is testable without a network.
//
//  Design notes worth keeping:
//
//    * The mirror is tried first and the official source is the fallback. A
//      Chinese network reaches BMCLAPI quickly and Mojang slowly; the reverse
//      happens on a VPN. Trying both costs one failed request, not a stuck app.
//
//    * Asset objects are content addressed, so a failure is retried per object
//      and a corrupt download is detected by SHA-1 instead of producing a pack
//      with a broken texture.
//
//    * The client jar is fetched too: `hz-class` reads the Java side to report
//      coverage, and a pack without it would silently claim 100% coverage.
//

import Foundation
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
    @Published var mirrorID: String = "bmclapi"
    @Published var selection = AssetSelection()
    @Published var onlyReleases = true

    private let session: URLSession
    private let fileManager = FileManager.default

    init() {
        let config = URLSessionConfiguration.default
        config.waitsForConnectivity = true
        config.timeoutIntervalForRequest = 30
        config.timeoutIntervalForResource = 600
        config.httpAdditionalHeaders = ["User-Agent": "HorizonPad"]
        session = URLSession(configuration: config)
    }

    var mirror: Mirror { Mirror.named(mirrorID) }

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

    // MARK: - One download

    /// Fetch, pack and port `version`. Returns the new pack's id.
    @discardableResult
    func download(_ version: RemoteVersion) async -> UUID? {
        guard !phase.isBusy else { return nil }
        let work = fileManager.temporaryDirectory
            .appendingPathComponent("hz-download-\(version.id)-\(UUID().uuidString)", isDirectory: true)
        do {
            try fileManager.createDirectory(at: work, withIntermediateDirectories: true)

            // 1. version metadata
            phase = .downloading(step: "版本信息", done: 0, total: 1)
            let metaData = try await getWithFallback(version.metadataURL)
            let info = try CatalogDecoder.version(from: metaData)

            // 2. asset index
            phase = .downloading(step: "资源索引", done: 0, total: 1)
            let indexData = try await getWithFallback(info.assetIndexURL)
            let objects = try CatalogDecoder.assetIndex(from: indexData)
            let plan = AssetPlanner.plan(objects: objects, selection: selection)
            guard !plan.isEmpty else {
                throw CatalogError.badJSON("asset selection produced no files")
            }

            // 3. client jar, into the pack as-is so the Java side is available
            phase = .downloading(step: "client.jar", done: 0, total: plan.count + 1)
            let jarURL = try await getWithFallback(info.clientJarURL)
            let jarBytes = try Data(contentsOf: jarURL)
            if info.clientJarSize > 0, jarBytes.count != info.clientJarSize {
                throw CatalogError.badJSON(
                    "client.jar 大小不符（\(jarBytes.count)/\(info.clientJarSize)）"
                )
            }

            // 4. asset objects, a few at a time
            let objects_dir = work.appendingPathComponent("objects", isDirectory: true)
            try fileManager.createDirectory(at: objects_dir, withIntermediateDirectories: true)
            try await fetchObjects(plan, into: objects_dir)

            // 5. pack everything the port needs
            phase = .packing
            var writer = StoreZipWriter()
            for asset in plan {
                let file = objects_dir.appendingPathComponent(asset.hash)
                guard let bytes = try? Data(contentsOf: file) else { continue }
                writer.add(path: asset.path, data: bytes)
            }
            let zipURL = work.appendingPathComponent("pack.zip")
            try writer.finalize().write(to: zipURL)

            // 6. port on the engine, exactly as a manual import does
            phase = .porting
            let id = UUID()
            let packURL = Paths.packURL(for: id)
            let reportURL = Paths.reportURL(for: id)
            let outcome: PortOutcome = try await withCheckedThrowingContinuation { continuation in
                Engine.queue.async {
                    do {
                        let result = try Porter.port(
                            jar: zipURL,
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
            let displayName = "Minecraft \(version.id)"
            let pack = InstalledPack(
                id: id,
                displayName: displayName,
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
            phase = .done(id)
            try? fileManager.removeItem(at: work)
            return id
        } catch {
            phase = .failed(describe(error))
            try? fileManager.removeItem(at: work)
            return nil
        }
    }

    // MARK: - Networking

    private func fetchObjects(_ plan: [SelectedAsset], into directory: URL) async throws {
        let total = plan.count
        var completed = 0
        let limit = 8

        try await withThrowingTaskGroup(of: Void.self) { group in
            var iterator = plan.makeIterator()
            var inFlight = 0

            func addNext() throws {
                guard let asset = iterator.next() else { return }
                inFlight += 1
                group.addTask { [weak self] in
                    guard let self else { return }
                    let bytes = try await self.getWithFallback(
                        ObjectURL.forHash(asset.hash, mirror: self.mirror)
                    )
                    let data = try Data(contentsOf: bytes)
                    let digest = InsecureSHA1.hex(data)
                    guard digest == asset.hash else {
                        throw CatalogError.badJSON("资源校验失败 \(asset.objectPath)")
                    }
                    try data.write(to: directory.appendingPathComponent(asset.hash))
                }
            }

            for _ in 0..<limit { try addNext() }

            while inFlight > 0 {
                try await group.next()
                inFlight -= 1
                completed += 1
                phase = .downloading(step: "资源", done: completed, total: total)
                try addNext()
            }
        }
    }

    private func get(_ url: URL) async throws -> Data {
        var request = URLRequest(url: url)
        request.timeoutInterval = 30
        let (data, response) = try await session.data(for: request)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw URLError(.badServerResponse)
        }
        return data
    }

    /// Tries the selected mirror, then the official source, then fails.
    private func getWithFallback(_ upstream: URL) async throws -> URL {
        let mirrorURL = mirror.rewrite(upstream)
        var candidates = [mirrorURL]
        if mirrorURL != upstream { candidates.append(upstream) }

        var lastError: Error?
        for candidate in candidates {
            do {
                let data = try await get(candidate)
                let saved = fileManager.temporaryDirectory
                    .appendingPathComponent(UUID().uuidString)
                try data.write(to: saved)
                return saved
            } catch {
                lastError = error
            }
        }
        throw lastError ?? URLError(.cannotLoadFromNetwork)
    }

    private func getWithFallbackData(_ upstream: URL) async throws -> Data {
        try Data(contentsOf: try await getWithFallback(upstream))
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