//
//  AppModel.swift
//  HorizonPad
//
//  Owns navigation state and the live game session.
//

import Combine
import Foundation
import Metal

@MainActor
final class AppModel: ObservableObject {
    enum Route: Hashable {
        case importJar
        case download
        case diagnostics
        case settings
        case packDetail(UUID)
        case play(UUID)
    }

    @Published var path: [Route] = []
    @Published var session: GameSession?
    @Published var lastError: String?
    @Published private(set) var metalAvailable: Bool = MTLCreateSystemDefaultDevice() != nil

    let library = Library.shared
    let settings = Settings.shared
    let log = LogStore.shared

    var device: MTLDevice? { MTLCreateSystemDefaultDevice() }

    func play(_ pack: InstalledPack) {
        guard let device else {
            lastError = "此设备不支持 Metal"
            return
        }
        let session = GameSession(packURL: pack.packURL, device: device)
        guard let session else {
            lastError = "无法启动会话"
            return
        }
        session.start()
        self.session = session
        library.markPlayed(pack.id)
        path.append(.play(pack.id))
    }

    func stopSession() {
        session?.stop()
        session = nil
    }

    /// Block names of the running session, for a block picker.
    var availableBlocks: [String] {
        session?.handle?.blockNames() ?? []
    }

    func saveWorld(for pack: InstalledPack, name: String) {
        guard let session else { return }
        let world = SavedWorld(packID: pack.id, name: name, bytes: 0)
        do {
            try session.handle?.save(to: world.url)
            let attributes = try? FileManager.default.attributesOfItem(atPath: world.url.path)
            let size = (attributes?[.size] as? NSNumber)?.intValue ?? 0
            var stored = world
            stored.bytes = size
            library.addWorld(stored)
            log.info("saved world \(name) (\(stored.bytes) bytes)")
        } catch {
            lastError = error.localizedDescription
        }
    }

    func loadWorld(_ world: SavedWorld) {
        guard let session else { return }
        session.load(from: world.url)
    }

    func environment() -> [String: String] {
        let info = Bundle.main.infoDictionary ?? [:]
        return [
            "app": info["CFBundleShortVersionString"] as? String ?? "?",
            "build": info["CFBundleVersion"] as? String ?? "?",
            "ios": ProcessInfo.processInfo.operatingSystemVersionString,
            "device": ProcessInfo.processInfo.hostName,
            "model": Self.hardwareModel(),
            "engine_api": String(Engine.apiVersion),
            "metal": device?.name ?? "unavailable",
        ]
    }

    static func hardwareModel() -> String {
        var info = utsname()
        uname(&info)
        let mirror = Mirror(reflecting: info.machine)
        let identifier = mirror.children.compactMap { value -> String? in
            guard let raw = value.value as? Int8, raw != 0 else { return nil }
            return String(UnicodeScalar(UInt8(raw)))
        }.joined()
        return identifier.isEmpty ? "unknown" : identifier
    }
}