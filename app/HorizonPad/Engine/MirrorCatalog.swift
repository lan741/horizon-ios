//
//  MirrorCatalog.swift
//  HorizonPad
//
//  Everything needed to describe and fetch a game version, kept free of
//  URLSession so it can be reasoned about (and tested) without a network.
//
//  Three facts drive the design:
//
//    1. A version needs two different downloads. The client jar carries the Java
//       side (`hz-class` reads it); the *assets* index carries the models,
//       textures and sounds, and those live as hash-named objects, not inside
//       the jar. A port without the asset index produces an empty world.
//
//    2. Mirrors are pure URL rewrites. BMCLAPI serves the same paths under its
//       own host, so a version's metadata and every object URL can be rewritten
//       mechanically instead of being carried in a second schema.
//
//    3. Only a subset of the assets matters. The port consumes blockstates,
//       models, block and item textures, language files, the sound catalogue and
//       (optionally) the sound payloads; pulling the whole index would mean
//       hundreds of megabytes of skins and paintings.
//

import Foundation

// MARK: - Mirrors

/// A download source. `rewrite` turns an upstream URL into this mirror's URL.
struct Mirror: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let note: String
    let manifestURL: URL
    /// Host prefixes this mirror shadows, each with the prefix that replaces it.
    /// Asset objects need `/assets` in front, metadata does not, so the mapping
    /// cannot be a single host swap.
    let rewrites: [PathRewrite]

    struct PathRewrite: Equatable, Sendable {
        let from: String
        let to: String
    }

    func rewrite(_ url: URL) -> URL {
        let text = url.absoluteString
        for rule in rewrites where text.hasPrefix(rule.from) {
            return URL(string: rule.to + text.dropFirst(rule.from.count)) ?? url
        }
        return url
    }

    static let all: [Mirror] = [
        Mirror(
            id: "bmclapi",
            name: "BMCLAPI 镜像",
            note: "国内直连，速度通常明显快于官方",
            manifestURL: URL(string: "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json")!,
            rewrites: [
                .init(from: "https://piston-meta.mojang.com", to: "https://bmclapi2.bangbang93.com"),
                .init(from: "https://piston-data.mojang.com", to: "https://bmclapi2.bangbang93.com"),
                .init(from: "https://launchermeta.mojang.com", to: "https://bmclapi2.bangbang93.com"),
                .init(from: "https://launcher.mojang.com", to: "https://bmclapi2.bangbang93.com"),
                // Objects live under /assets on the mirror, not at its root.
                .init(
                    from: "https://resources.download.minecraft.net",
                    to: "https://bmclapi2.bangbang93.com/assets"
                ),
            ]
        ),
        Mirror(
            id: "official",
            name: "官方源",
            note: "piston-meta.mojang.com，海外网络更稳",
            manifestURL: URL(string: "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")!,
            rewrites: []
        ),
    ]

    static func named(_ id: String) -> Mirror {
        all.first { $0.id == id } ?? all[0]
    }
}

// MARK: - Manifest

struct RemoteVersion: Identifiable, Equatable, Sendable {
    let id: String
    let type: String
    let releaseTime: String
    let metadataURL: URL

    var isRelease: Bool { type == "release" }
}

struct VersionManifest: Equatable, Sendable {
    let latestRelease: String
    let latestSnapshot: String
    let versions: [RemoteVersion]

    var releases: [RemoteVersion] { versions.filter(\.isRelease) }
}

enum CatalogError: LocalizedError {
    case badJSON(String)
    case missingField(String)
    case emptyManifest

    var errorDescription: String? {
        switch self {
        case .badJSON(let what): return "无法解析 \(what)"
        case .missingField(let field): return "缺少字段 \(field)"
        case .emptyManifest: return "版本清单为空"
        }
    }
}

enum CatalogDecoder {
    static func manifest(from data: Data) throws -> VersionManifest {
        guard
            let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let latest = root["latest"] as? [String: Any],
            let versions = root["versions"] as? [[String: Any]]
        else {
            throw CatalogError.badJSON("version_manifest_v2.json")
        }
        let parsed: [RemoteVersion] = versions.compactMap { entry in
            guard
                let id = entry["id"] as? String,
                let type = entry["type"] as? String,
                let urlText = entry["url"] as? String,
                let url = URL(string: urlText)
            else { return nil }
            return RemoteVersion(
                id: id,
                type: type,
                releaseTime: entry["releaseTime"] as? String ?? "",
                metadataURL: url
            )
        }
        guard !parsed.isEmpty else { throw CatalogError.emptyManifest }
        return VersionManifest(
            latestRelease: latest["release"] as? String ?? "",
            latestSnapshot: latest["snapshot"] as? String ?? "",
            versions: parsed
        )
    }

    /// The parts of a version's metadata the port needs.
    struct VersionInfo: Equatable, Sendable {
        let id: String
        let clientJarURL: URL
        let clientJarSize: Int
        let clientJarSHA1: String
        let assetIndexURL: URL
        let assetIndexID: String
    }

    static func version(from data: Data) throws -> VersionInfo {
        guard
            let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let id = root["id"] as? String,
            let downloads = root["downloads"] as? [String: Any],
            let client = downloads["client"] as? [String: Any],
            let jarURLText = client["url"] as? String,
            let jarURL = URL(string: jarURLText),
            let index = root["assetIndex"] as? [String: Any],
            let indexURLText = index["url"] as? String,
            let indexURL = URL(string: indexURLText)
        else {
            throw CatalogError.badJSON("version json")
        }
        return VersionInfo(
            id: id,
            clientJarURL: jarURL,
            clientJarSize: client["size"] as? Int ?? 0,
            clientJarSHA1: client["sha1"] as? String ?? "",
            assetIndexURL: indexURL,
            assetIndexID: index["id"] as? String ?? id
        )
    }

    struct AssetEntry: Equatable, Sendable {
        let hash: String
        let size: Int
    }

    static func assetIndex(from data: Data) throws -> [String: AssetEntry] {
        guard
            let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let objects = root["objects"] as? [String: Any]
        else {
            throw CatalogError.badJSON("asset index")
        }
        var out: [String: AssetEntry] = [:]
        out.reserveCapacity(objects.count)
        for (path, value) in objects {
            guard
                let entry = value as? [String: Any],
                let hash = entry["hash"] as? String
            else { continue }
            out[path] = AssetEntry(hash: hash, size: entry["size"] as? Int ?? 0)
        }
        return out
    }
}

// MARK: - Which assets to fetch

struct AssetSelection: Equatable, Sendable {
    var includeSounds = false
    var includeShaders = false
    /// Languages to carry; the app falls back to en_us when one is missing.
    var languages: Set<String> = ["en_us", "zh_cn"]
}

struct SelectedAsset: Equatable, Sendable {
    /// Path inside the pack, e.g. `assets/minecraft/blockstates/stone.json`.
    let path: String
    /// Path in the object store: `minecraft/blockstates/stone.json`.
    let objectPath: String
    let hash: String
    let size: Int
}

enum AssetPlanner {
    /// Prefixes the port always needs, in the order they should be fetched.
    static let requiredPrefixes = [
        "minecraft/blockstates/",
        "minecraft/models/",
        "minecraft/textures/block/",
        "minecraft/textures/item/",
        "minecraft/font/",
    ]

    static func plan(
        objects: [String: CatalogDecoder.AssetEntry],
        selection: AssetSelection
    ) -> [SelectedAsset] {
        var out: [SelectedAsset] = []
        out.reserveCapacity(objects.count / 4)

        func wants(_ path: String) -> Bool {
            if requiredPrefixes.contains(where: path.hasPrefix) { return true }
            if path == "minecraft/sounds.json" { return true }
            if path.hasPrefix("minecraft/lang/") {
                let name = path.dropFirst("minecraft/lang/".count)
                let stem = name.replacingOccurrences(of: ".json", with: "")
                return selection.languages.contains(stem)
            }
            if selection.includeShaders, path.hasPrefix("minecraft/shaders/") { return true }
            if selection.includeSounds, path.hasPrefix("minecraft/sounds/") {
                // The catalogue lists every sound; the payloads are the bulk.
                return path.hasSuffix(".ogg")
            }
            return false
        }

        for (path, entry) in objects where wants(path) {
            out.append(
                SelectedAsset(
                    path: "assets/" + path,
                    objectPath: path,
                    hash: entry.hash,
                    size: entry.size
                )
            )
        }
        out.sort { $0.path < $1.path }
        return out
    }

    static func totalBytes(_ plan: [SelectedAsset]) -> Int {
        plan.reduce(0) { $0 + $1.size }
    }
}

// MARK: - Object URLs

enum ObjectURL {
    /// Objects live at `resources.download.minecraft.net/<hh>/<hash>`; the mirror
    /// shadows that host with `/assets`.
    static func forHash(_ hash: String, mirror: Mirror) -> URL {
        let prefix = String(hash.prefix(2))
        let upstream = URL(string: "https://resources.download.minecraft.net/\(prefix)/\(hash)")!
        return mirror.rewrite(upstream)
    }
}

// MARK: - Store-only ZIP

/// Writes a ZIP with no compression.
///
/// The port pipeline takes a jar/zip, so the downloaded assets have to be packed
/// before the engine can see them. Storing rather than deflating is deliberate:
/// this runs on a phone right after a download that already spent the network,
/// and a few hundred megabytes of stored PNGs compress by only a few percent
/// while costing a full CPU pass.
struct StoreZipWriter {
    private struct Entry {
        let name: [UInt8]
        let crc: UInt32
        let size: UInt32
        let offset: UInt32
    }

    private var body = Data()
    private var entries: [Entry] = []
    private let dosTime: UInt16
    private let dosDate: UInt16

    /// A fixed timestamp keeps two runs byte-identical, which the engine's
    /// determinism test relies on.
    init(date: Date = Date(timeIntervalSince1970: 1_700_000_000)) {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        let parts = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
        let year = max(1980, parts.year ?? 2023) - 1980
        dosTime = UInt16((parts.hour ?? 0) << 11 | (parts.minute ?? 0) << 5 | (parts.second ?? 0) / 2)
        dosDate = UInt16(year << 9 | (parts.month ?? 1) << 5 | (parts.day ?? 1))
    }

    mutating func add(path: String, data: Data) {
        let name = Array(path.utf8)
        let crc = CRC32.checksum(data)
        let offset = UInt32(body.count)

        body.append(contentsOf: [0x50, 0x4B, 0x03, 0x04])            // local header
        body.append(uint16: 20)                                      // version needed
        body.append(uint16: 0)                                       // flags
        body.append(uint16: 0)                                       // method: store
        body.append(uint16: dosTime)
        body.append(uint16: dosDate)
        body.append(uint32: crc)
        body.append(uint32: UInt32(data.count))
        body.append(uint32: UInt32(data.count))
        body.append(uint16: UInt16(name.count))
        body.append(uint16: 0)                                       // extra length
        body.append(contentsOf: name)
        body.append(data)

        entries.append(Entry(name: name, crc: crc, size: UInt32(data.count), offset: offset))
    }

    var count: Int { entries.count }
    var uncompressedBytes: Int { entries.reduce(0) { $0 + Int($1.size) } }

    func finalize() -> Data {
        var out = body
        let directoryStart = UInt32(out.count)
        for entry in entries {
            out.append(contentsOf: [0x50, 0x4B, 0x01, 0x02])        // central directory
            out.append(uint16: 20)                                   // version made by
            out.append(uint16: 20)                                   // version needed
            out.append(uint16: 0)                                    // flags
            out.append(uint16: 0)                                    // method
            out.append(uint16: dosTime)
            out.append(uint16: dosDate)
            out.append(uint32: entry.crc)
            out.append(uint32: entry.size)
            out.append(uint32: entry.size)
            out.append(uint16: UInt16(entry.name.count))
            out.append(uint16: 0)                                    // extra
            out.append(uint16: 0)                                    // comment
            out.append(uint16: 0)                                    // disk
            out.append(uint16: 0)                                    // internal attrs
            out.append(uint32: 0)                                    // external attrs
            out.append(uint32: entry.offset)
            out.append(contentsOf: entry.name)
        }
        let directorySize = UInt32(out.count) - directoryStart
        out.append(contentsOf: [0x50, 0x4B, 0x05, 0x06])            // end of central directory
        out.append(uint16: 0)                                        // disk
        out.append(uint16: 0)                                        // disk with directory
        out.append(uint16: UInt16(entries.count))
        out.append(uint16: UInt16(entries.count))
        out.append(uint32: directorySize)
        out.append(uint32: directoryStart)
        out.append(uint16: 0)                                        // comment length
        return out
    }
}

enum CRC32 {
    private static let table: [UInt32] = (0..<256).map { index -> UInt32 in
        var value = UInt32(index)
        for _ in 0..<8 {
            value = (value & 1) != 0 ? (0xEDB8_8320 ^ (value >> 1)) : (value >> 1)
        }
        return value
    }

    static func checksum(_ data: Data) -> UInt32 {
        var crc: UInt32 = 0xFFFF_FFFF
        for byte in data {
            crc = table[Int((crc ^ UInt32(byte)) & 0xFF)] ^ (crc >> 8)
        }
        return crc ^ 0xFFFF_FFFF
    }
}

// MARK: - Small helpers

extension Data {
    mutating func append(uint16 value: UInt16) {
        append(contentsOf: [UInt8(value & 0xFF), UInt8((value >> 8) & 0xFF)])
    }

    mutating func append(uint32 value: UInt32) {
        append(
            contentsOf: [
                UInt8(value & 0xFF),
                UInt8((value >> 8) & 0xFF),
                UInt8((value >> 16) & 0xFF),
                UInt8((value >> 24) & 0xFF),
            ]
        )
    }
}

/// Human-readable byte count for the UI.
enum ByteText {
    static func string(_ bytes: Int) -> String {
        let units = ["B", "KB", "MB", "GB"]
        var value = Double(bytes)
        var unit = 0
        while value >= 1024, unit < units.count - 1 {
            value /= 1024
            unit += 1
        }
        return unit == 0
            ? "\(bytes) B"
            : String(format: "%.1f %@", value, units[unit])
    }
}