//
//  Paths.swift
//  HorizonPad
//
//  Everything lives in the app container. No iCloud, no shared container:
//  imported jars are the user's own copy of a game they own, and we keep them
//  local unless the user exports them.
//

import Foundation

enum Paths {
    static let root: URL = {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("HorizonPad", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }()

    static var jars: URL { directory("Jars") }
    static var packs: URL { directory("Packs") }
    static var reports: URL { directory("Reports") }
    static var worlds: URL { directory("Worlds") }
    static var thumbs: URL { directory("Thumbnails") }

    private static func directory(_ name: String) -> URL {
        let dir = root.appendingPathComponent(name, isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    static func packURL(for id: UUID) -> URL {
        packs.appendingPathComponent("\(id.uuidString).hzp")
    }

    static func reportURL(for id: UUID) -> URL {
        reports.appendingPathComponent("\(id.uuidString).json")
    }

    static func worldURL(for id: UUID) -> URL {
        worlds.appendingPathComponent("\(id.uuidString).hzw")
    }

    /// Copy a security-scoped file into the container. Returns the new URL.
    /// The caller is responsible for `startAccessingSecurityScopedResource`.
    static func importCopy(of source: URL, into dir: URL, preferredName: String? = nil) throws -> URL {
        let name = preferredName ?? source.lastPathComponent
        let destination = dir.appendingPathComponent(name)
        if FileManager.default.fileExists(atPath: destination.path) { return destination }
        try FileManager.default.copyItem(at: source, to: destination)
        return destination
    }
}