//
//  DownloadStore.swift
//  HorizonPad
//
//  Where downloaded bytes live between attempts.
//
//  The first version of the downloader put everything in a UUID-named temporary
//  directory and deleted it on any error, so a dropped connection at 90% of a
//  41 MB client jar meant starting over. Everything here exists to make progress
//  survive an interruption.
//
//  Rules:
//
//    * one directory per version, stable across attempts and app launches;
//    * asset objects are stored under their hash, so an object either exists
//      completely or not at all — there is no partially-written object to
//      mistake for a finished one, because writes go through a `.part` file;
//    * nothing is deleted on failure. A failed download leaves the store as
//      progress, and the UI can clear it explicitly.
//
//  The resume logic itself (deciding append vs restart from a range response) is
//  a plain function so it can be tested without a network.
//

import Foundation

/// What to do with a file we may already have partially.
enum ResumeDecision: Equatable {
    /// Already complete.
    case done
    /// Fetch the remaining bytes starting at this offset.
    case append(from: Int)
    /// Start over (nothing cached, or the remote ignored the range).
    case restart

    var startOffset: Int {
        switch self {
        case .done: return 0
        case .append(let offset): return offset
        case .restart: return 0
        }
    }
}

enum ResumePlanner {
    /// Decide what to do given what is on disk and what the server allows.
    ///
    /// - Parameters:
    ///   - cachedSize: bytes already on disk (0 when absent)
    ///   - expectedSize: total size, or 0 when unknown
    ///   - serverStatus: HTTP status for a `Range` request, or 0 when no range
    ///     request was made yet
    static func decide(
        cachedSize: Int,
        expectedSize: Int,
        serverStatus: Int
    ) -> ResumeDecision {
        guard cachedSize > 0 else { return .restart }
        if expectedSize > 0, cachedSize == expectedSize { return .done }
        if expectedSize > 0, cachedSize > expectedSize { return .restart }
        // 206 Partial Content: the server honoured the range.
        if serverStatus == 206 { return .append(from: cachedSize) }
        // 200 or anything else: it sent the whole file, so the partial copy is
        // useless and appending would corrupt the result.
        if serverStatus != 0 || cachedSize > 0 { return .restart }
        return .restart
    }
}

/// A per-version download directory with an object store.
struct DownloadStore {
    let root: URL

    private let fileManager = FileManager.default

    init(versionID: String) {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        root = caches
            .appendingPathComponent("horizonpad/downloads", isDirectory: true)
            .appendingPathComponent(Self.safeName(versionID), isDirectory: true)
    }

    /// Version ids contain dots and `-`; keep them readable but filesystem-safe.
    static func safeName(_ id: String) -> String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: ".-_"))
        let filtered = id.unicodeScalars.map { allowed.contains($0) ? Character($0) : "_" }
        return String(filtered)
    }

    var objectsDirectory: URL { root.appendingPathComponent("objects", isDirectory: true) }

    func prepare() throws {
        try fileManager.createDirectory(at: objectsDirectory, withIntermediateDirectories: true)
    }

    func objectURL(_ hash: String) -> URL { objectsDirectory.appendingPathComponent(hash) }

    /// True when the object is present with the recorded size.
    ///
    /// Size is enough here because objects are written atomically and a size
    /// mismatch means a truncated leftover from an older, non-atomic version.
    /// The SHA-1 is still checked once per object on first fetch.
    func hasObject(_ hash: String, expectedSize: Int) -> Bool {
        let url = objectURL(hash)
        guard let attributes = try? fileManager.attributesOfItem(atPath: url.path),
              let size = attributes[.size] as? Int
        else { return false }
        return expectedSize <= 0 || size == expectedSize
    }

    func countObjects() -> Int {
        (try? fileManager.contentsOfDirectory(atPath: objectsDirectory.path))?.count ?? 0
    }

    /// Write an object atomically: a crash mid-write cannot leave a file that
    /// looks complete.
    func stashObject(_ data: Data, hash: String) throws {
        let destination = objectURL(hash)
        let temporary = destination.appendingPathExtension("part")
        try data.write(to: temporary, options: .atomic)
        if fileManager.fileExists(atPath: destination.path) {
            try? fileManager.removeItem(at: destination)
        }
        try fileManager.moveItem(at: temporary, to: destination)
    }

    func readObject(_ hash: String) throws -> Data {
        try Data(contentsOf: objectURL(hash))
    }

    /// The assembled pack, kept so a retry does not rebuild what already ported.
    var packURL: URL { root.appendingPathComponent("pack.zip") }
    var hzpURL: URL { root.appendingPathComponent("pack.hzp") }

    func removeIntermediate() {
        try? fileManager.removeItem(at: packURL)
    }

    func clear() throws {
        try? fileManager.removeItem(at: root)
    }

    var byteCount: Int {
        guard let enumerator = fileManager.enumerator(
            at: root,
            includingPropertiesForKeys: [.fileSizeKey]
        ) else { return 0 }
        var total = 0
        for case let url as URL in enumerator {
            let size = (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
            total += size
        }
        return total
    }
}