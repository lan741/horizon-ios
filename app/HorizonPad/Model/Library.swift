//
//  Library.swift
//  HorizonPad
//
//  Installed packs + worlds, persisted as JSON in the container. Small enough
//  that a single file is better than a database: it is readable, diffable and
//  can be exported for a bug report.
//

import Combine
import Foundation

struct InstalledPack: Identifiable, Codable, Hashable, Sendable {
    var id: UUID = UUID()
    var displayName: String
    var minecraftVersion: String
    var engineAPI: Int
    var createdAt: Date = Date()
    var lastPlayed: Date?
    var packBytes: Int
    var entryCount: Int
    /// Kind → count, straight from the engine's summary.
    var kinds: [String: Int]
    var manifestSHA256: String
    var coverage: Double
    var javaClasses: Int
    /// Kept so the UI can explain what is still Java without re-parsing.
    var jvmSummary: JVMSummary?

    var packURL: URL { Paths.packURL(for: id) }
    var reportURL: URL { Paths.reportURL(for: id) }
    var worldURL: URL { Paths.worldURL(for: id) }

    var installed: Bool {
        FileManager.default.fileExists(atPath: packURL.path)
    }
}

struct JVMSummary: Codable, Hashable, Sendable {
    var classes: Int
    var methods: Int
    var bytecodeBytes: Int
    var callEdges: Int
    var topTargets: [JVMTarget]

    init(json: [String: Any]) {
        classes = json["classes"] as? Int ?? 0
        methods = json["methods"] as? Int ?? 0
        bytecodeBytes = json["bytecode_bytes"] as? Int ?? 0
        callEdges = json["call_edges"] as? Int ?? 0
        let hot = json["hot_targets"] as? [[String: Any]] ?? []
        topTargets = hot.prefix(12).map { entry in
            JVMTarget(
                owner: (entry["owner"] as? String ?? "?").replacingOccurrences(of: "/", with: "."),
                method: entry["method"] as? String ?? "?",
                callSites: entry["call_sites"] as? Int ?? 0
            )
        }
    }
}

struct JVMTarget: Codable, Hashable, Sendable, Identifiable {
    var owner: String
    var method: String
    var callSites: Int
    var id: String { "\(owner)#\(method)" }
}

struct SavedWorld: Identifiable, Codable, Hashable, Sendable {
    var id: UUID = UUID()
    var packID: UUID
    var name: String
    var savedAt: Date = Date()
    var bytes: Int

    var url: URL { Paths.worldURL(for: id) }
}

@MainActor
final class Library: ObservableObject {
    static let shared = Library()

    @Published private(set) var packs: [InstalledPack] = []
    @Published private(set) var worlds: [SavedWorld] = []

    private let storeURL = Paths.root.appendingPathComponent("library.json")

    private struct Store: Codable {
        var packs: [InstalledPack]
        var worlds: [SavedWorld]
    }

    init() {
        load()
    }

    private func load() {
        guard
            let data = try? Data(contentsOf: storeURL),
            let store = try? JSONDecoder().decode(Store.self, from: data)
        else { return }
        packs = store.packs
        worlds = store.worlds
    }

    private func persist() {
        let store = Store(packs: packs, worlds: worlds)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        guard let data = try? encoder.encode(store) else { return }
        try? data.write(to: storeURL, options: .atomic)
    }

    func add(_ pack: InstalledPack) {
        packs.removeAll { $0.id == pack.id }
        packs.insert(pack, at: 0)
        persist()
    }

    func update(_ pack: InstalledPack) {
        guard let index = packs.firstIndex(where: { $0.id == pack.id }) else { return }
        packs[index] = pack
        persist()
    }

    func markPlayed(_ id: UUID) {
        guard let index = packs.firstIndex(where: { $0.id == id }) else { return }
        packs[index].lastPlayed = Date()
        persist()
    }

    /// Removes the pack, its report and every world saved against it.
    func remove(_ id: UUID) {
        guard let pack = packs.first(where: { $0.id == id }) else { return }
        for world in worlds where world.packID == id {
            try? FileManager.default.removeItem(at: world.url)
        }
        worlds.removeAll { $0.packID == id }
        try? FileManager.default.removeItem(at: pack.packURL)
        try? FileManager.default.removeItem(at: pack.reportURL)
        packs.removeAll { $0.id == id }
        persist()
    }

    func addWorld(_ world: SavedWorld) {
        worlds.insert(world, at: 0)
        persist()
    }

    func removeWorld(_ id: UUID) {
        if let world = worlds.first(where: { $0.id == id }) {
            try? FileManager.default.removeItem(at: world.url)
        }
        worlds.removeAll { $0.id == id }
        persist()
    }

    func worlds(for pack: UUID) -> [SavedWorld] {
        worlds.filter { $0.packID == pack }
    }

    var totalBytes: Int {
        packs.reduce(0) { $0 + $1.packBytes }
    }
}