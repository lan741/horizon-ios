//
//  HZEngine.swift
//  HorizonPad
//
//  The only place that talks to the Rust engine. Everything above this file
//  works with Swift types; everything below is a C ABI.
//
//  Threading contract: porting and simulation run on a private serial queue.
//  Callers on the main actor await the async wrappers; the synchronous entry
//  points are `nonisolated` and documented as blocking.
//

import Foundation

// MARK: - Errors

enum EngineError: LocalizedError {
    case portFailed(String)
    case packRejected(String)
    case badPath(String)

    var errorDescription: String? {
        switch self {
        case .portFailed(let m): "导入失败：\(m)"
        case .packRejected(let m): "无法加载资源包：\(m)"
        case .badPath(let p): "路径无效：\(p)"
        }
    }
}

// MARK: - Progress

/// Stages, mirrored from `hz_port::Stage`. The order matters: it is the order
/// the C callback uses.
enum PortStage: Int, CaseIterable {
    case read, blockstates, models, textures, lang, data, mixins, classes, write
}

struct PortProgress: Sendable {
    var stage: PortStage = .read
    var done: UInt32 = 0
    var total: UInt32 = 0

    var fraction: Double {
        guard total > 0 else { return 0 }
        // Each stage is equal weight; the reported fraction is monotonic.
        let stages = Double(PortStage.allCases.count)
        let base = Double(stage.rawValue)
        return min(1.0, (base + Double(done) / Double(total)) / stages)
    }

    var label: String {
        switch stage {
        case .read: "读取 jar"
        case .blockstates: "编译方块状态"
        case .models: "烘焙模型几何"
        case .textures: "打包贴图"
        case .lang: "编译语言表"
        case .data: "规范化数据包"
        case .mixins: "收集 mixin 描述"
        case .classes: "清点 Java 类"
        case .write: "写出资源包"
        }
    }
}

/// Box passed to the C callback; `Unmanaged` keeps it alive for the call.
private final class ProgressBox {
    let emit: @Sendable (PortProgress) -> Void
    init(emit: @escaping @Sendable (PortProgress) -> Void) { self.emit = emit }
}

private let progressCallback: HzProgress = { ctx, stagePtr, done, total in
    guard let ctx else { return }
    let box = Unmanaged<ProgressBox>.fromOpaque(ctx).takeUnretainedValue()
    let raw = stagePtr.map { String(cString: $0) } ?? "read"
    let stage = PortStage.allCases.first { name(for: $0) == raw } ?? .read
    box.emit(PortProgress(stage: stage, done: done, total: total))
}

private func name(for stage: PortStage) -> String {
    switch stage {
    case .read: "read"
    case .blockstates: "blockstates"
    case .models: "models"
    case .textures: "textures"
    case .lang: "lang"
    case .data: "data"
    case .mixins: "mixins"
    case .classes: "classes"
    case .write: "write"
    }
}

// MARK: - Engine

enum Engine {
    /// Serial queue for every engine call that touches a handle.
    static let queue = DispatchQueue(label: "dev.playhorizon.pad.engine", qos: .userInitiated)

    static var apiVersion: UInt32 { hz_api_version() }

    static func lastError() -> String {
        guard let p = hz_last_error() else { return "unknown engine error" }
        return String(cString: p)
    }

    static func summary(ofPackAt path: String) throws -> PackSummary {
        guard let raw = path.withCString({ hz_pack_summary($0) }) else {
            throw EngineError.packRejected(lastError())
        }
        defer { hz_string_free(raw) }
        let json = String(cString: raw)
        return try PackSummary(json: json)
    }
}

// MARK: - Pack summary

struct PackSummary: Sendable {
    var entries: Int
    var kinds: [String: Int]
    var manifestSHA256: String
    var bytes: Int

    init(json: String) throws {
        guard
            let data = json.data(using: .utf8),
            let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            throw EngineError.packRejected("引擎返回了无法解析的摘要")
        }
        entries = obj["entries"] as? Int ?? 0
        kinds = obj["kinds"] as? [String: Int] ?? [:]
        manifestSHA256 = obj["manifest_sha256"] as? String ?? "none"
        bytes = obj["bytes"] as? Int ?? 0
    }
}

// MARK: - Porting

struct PortOutcome: Sendable {
    var pack: PackSummary
    var reportJSON: String
}

enum Porter {
    /// Port `jar` into `packURL`, writing the JSON report beside it.
    /// Blocking: call from `Engine.queue`.
    static func port(jar: URL,
                     pack: URL,
                     report: URL,
                     minecraftVersion: String,
                     progress: @escaping @Sendable (PortProgress) -> Void) throws -> PortOutcome {
        let box = ProgressBox(emit: progress)
        let ctx = Unmanaged.passUnretained(box).toOpaque()
        let status = jar.path.withCString { jarC in
            pack.path.withCString { packC in
                report.path.withCString { reportC in
                    minecraftVersion.withCString { versionC in
                        hz_port_jar(jarC, packC, reportC, versionC, progressCallback, ctx)
                    }
                }
            }
        }
        withExtendedLifetime(box) {}
        if status != 0 {
            throw EngineError.portFailed(Engine.lastError())
        }
        let summary = try Engine.summary(ofPackAt: pack.path)
        let reportJSON = (try? String(contentsOf: report, encoding: .utf8)) ?? "{}"
        return PortOutcome(pack: summary, reportJSON: reportJSON)
    }
}

// MARK: - Game handle

/// Owns a `HzGame *`. `deinit` frees it, so one instance == one session.
final class GameHandle {
    // Defaulted so the failable init can return nil after a partial attempt;
    // a class initialiser may not be the reason a property stays unset.
    private(set) var raw: OpaquePointer? = nil
    /// Width of one vertex in floats; matches `hz_game::frame::FLOATS_PER_VERTEX`.
    static let floatsPerVertex = 16

    init(packURL: URL) throws {
        guard let handle = packURL.path.withCString({ hz_game_new($0) }) else {
            throw EngineError.packRejected(Engine.lastError())
        }
        raw = handle
    }

    deinit {
        if let raw { hz_game_free(raw) }
    }

    func tick(_ dt: Float) {
        guard let raw else { return }
        hz_game_tick(raw, dt)
    }

    /// Returns true when the vertex buffer changed; callers skip the copy
    /// otherwise (an idle frame is then a single comparison).
    @discardableResult
    func rebuildFrame() -> Bool {
        guard let raw else { return false }
        return hz_game_rebuild_frame(raw) != 0
    }

    var quadCount: UInt32 {
        guard let raw else { return 0 }
        return hz_game_frame_quads(raw)
    }

    // MARK: Per-section vertices

    /// One drawable section: its identity, vertex count and a generation stamp
    /// that changes whenever its geometry is re-baked.
    struct Section: Sendable {
        var x: Int32
        var y: Int32
        var z: Int32
        var vertexCount: Int
        var generation: UInt32
        var key: String { "\(x),\(y),\(z)" }
    }

    /// Re-bake without rebuilding the flat frame. Returns 0 when nothing moved.
    @discardableResult
    func rebuildMeshes() -> UInt32 {
        guard let raw else { return 0 }
        return hz_game_rebuild_meshes(raw)
    }

    /// Sections the renderer should draw right now, in stable order.
    func visibleSections() -> [Section] {
        guard let raw else { return [] }
        let count = Int(hz_game_section_count(raw))
        guard count > 0 else { return [] }
        var out: [Section] = []
        out.reserveCapacity(count)
        var fields = [UInt32](repeating: 0, count: 5)
        for index in 0..<count {
            let got = fields.withUnsafeMutableBufferPointer { ptr -> UInt32 in
                guard let base = ptr.baseAddress else { return 0 }
                return hz_game_section_at(raw, UInt32(index), base)
            }
            guard got == 5 else { continue }
            out.append(Section(
                x: Int32(bitPattern: fields[0]),
                y: Int32(bitPattern: fields[1]),
                z: Int32(bitPattern: fields[2]),
                vertexCount: Int(fields[3]),
                generation: fields[4]
            ))
        }
        return out
    }

    /// Vertices of one section. `nil` when it has no cached geometry.
    func sectionVertices(_ section: Section) -> [Float]? {
        guard let raw, section.vertexCount > 0 else { return nil }
        let floats = section.vertexCount * Self.floatsPerVertex
        var buffer = [Float](repeating: 0, count: floats)
        let written = buffer.withUnsafeMutableBufferPointer { ptr -> UInt32 in
            guard let base = ptr.baseAddress else { return 0 }
            return hz_game_section_vertices(raw, section.x, section.y, section.z,
                                            base, UInt32(ptr.count))
        }
        guard written > 0 else { return nil }
        return Array(buffer.prefix(Int(written)))
    }

    /// Bytes the flat assembler would copy for the current view.
    var flatFrameBytes: Int {
        guard let raw else { return 0 }
        return Int(hz_game_flat_frame_bytes(raw))
    }

    /// Copy the interleaved vertex buffer. Returns an empty array when the
    /// engine has nothing to draw (or the handle is gone).
    func frameVertices() -> [Float] {
        guard let raw else { return [] }
        var capacity = Int(quadCount) * 6 * Self.floatsPerVertex
        if capacity == 0 { capacity = 6 * Self.floatsPerVertex }
        var buffer = [Float](repeating: 0, count: capacity)
        let written = buffer.withUnsafeMutableBufferPointer { ptr -> UInt32 in
            guard let base = ptr.baseAddress else { return 0 }
            return hz_game_frame_data(raw, base, UInt32(ptr.count))
        }
        if written == 0 { return [] }
        return Array(buffer.prefix(Int(written)))
    }

    func setPlayer(x: Float, y: Float, z: Float, yaw: Float, pitch: Float) {
        guard let raw else { return }
        hz_game_set_player(raw, x, y, z, yaw, pitch)
    }

    var playerPosition: SIMD3<Float> {
        guard let raw else { return .zero }
        var out = [Float](repeating: 0, count: 3)
        let n = out.withUnsafeMutableBufferPointer { ptr in
            hz_game_player_pos(raw, ptr.baseAddress)
        }
        guard n == 3 else { return .zero }
        return SIMD3(out[0], out[1], out[2])
    }

    func setBlock(x: Int32, y: Int32, z: Int32, id: UInt32) {
        guard let raw else { return }
        hz_game_set_block(raw, x, y, z, id)
    }

    /// One hit from a voxel raycast, in the engine's integer block space.
    struct RayHit: Sendable {
        var block: SIMD3<Int32>
        var normal: SIMD3<Int32>
        var distance: Float

        /// Where a placed block goes.
        var adjacent: SIMD3<Int32> { block &+ normal }
    }

    /// First solid block along the ray, or nil.
    func raycast(origin: SIMD3<Float>,
                 direction: SIMD3<Float>,
                 maxDistance: Float = 6) -> RayHit? {
        guard let raw else { return nil }
        var out = [Float](repeating: 0, count: 7)
        let n = out.withUnsafeMutableBufferPointer { ptr -> UInt32 in
            guard let base = ptr.baseAddress else { return 0 }
            return hz_game_raycast(raw,
                                   origin.x, origin.y, origin.z,
                                   direction.x, direction.y, direction.z,
                                   maxDistance, base)
        }
        guard n == 7 else { return nil }
        return RayHit(
            block: SIMD3(Int32(out[0]), Int32(out[1]), Int32(out[2])),
            normal: SIMD3(Int32(out[3]), Int32(out[4]), Int32(out[5])),
            distance: out[6]
        )
    }

    /// Dense id of a named block, or nil when the pack lacks it.
    func blockID(_ name: String) -> UInt32? {
        guard let raw else { return nil }
        let id = name.withCString { hz_game_block_id(raw, $0) }
        return id == UInt32.max ? nil : id
    }

    var blockCount: Int {
        guard let raw else { return 0 }
        return Int(hz_game_block_count(raw))
    }

    /// Every block name in pack order; used by the block picker.
    func blockNames() -> [String] {
        guard let raw else { return [] }
        let count = blockCount
        guard count > 0 else { return [] }
        var names: [String] = []
        names.reserveCapacity(count)
        var buffer = [CChar](repeating: 0, count: 160)
        for i in 0..<count {
            let written = buffer.withUnsafeMutableBufferPointer { ptr -> UInt32 in
                guard let base = ptr.baseAddress else { return 0 }
                return hz_game_block_name(raw, UInt32(i), base, UInt32(ptr.count))
            }
            names.append(written > 0 ? String(cString: buffer) : "block#\(i)")
        }
        return names
    }

    // MARK: Light

    /// Daylight for the sky channel: 0 night, 1 noon. The engine re-bakes the
    /// affected sections on the next frame.
    var daylight: Float {
        get {
            guard let raw else { return 1 }
            return hz_game_daylight(raw)
        }
        set {
            guard let raw else { return }
            hz_game_set_daylight(raw, newValue)
        }
    }

    /// The three light numbers at a block position, each 0...15.
    struct LightSample: Sendable {
        var sky: UInt32
        var block: UInt32
        var brightness: UInt32
    }

    func light(at position: SIMD3<Int32>) -> LightSample {
        guard let raw else { return LightSample(sky: 0, block: 0, brightness: 0) }
        return LightSample(
            sky: hz_game_sky_at(raw, position.x, position.y, position.z),
            block: hz_game_block_light_at(raw, position.x, position.y, position.z),
            brightness: hz_game_brightness_at(raw, position.x, position.y, position.z)
        )
    }

    // MARK: Sounds

    /// Facts about one sound payload, straight from the Ogg container.
    struct SoundInfo: Sendable {
        var assetCount: Int
        var eventCount: Int
    }

    var soundInfo: SoundInfo {
        guard let raw else { return SoundInfo(assetCount: 0, eventCount: 0) }
        return SoundInfo(assetCount: Int(hz_game_sound_asset_count(raw)),
                         eventCount: Int(hz_game_sound_event_count(raw)))
    }

    /// Ask the engine to play an event at a world position. The engine resolves
    /// the variant, attenuates by distance and queues the request.
    @discardableResult
    func play(event: String, at position: SIMD3<Float>) -> Bool {
        guard let raw else { return false }
        return event.withCString { name in
            hz_game_play_sound(raw, name, position.x, position.y, position.z) == 1
        }
    }

    // MARK: Shader packs

    struct ShaderPack: Identifiable, Sendable {
        var id: String
        var summary: String
        var supported: Bool
        /// Index into the engine's pack list, for `shaderMSL(index:)`.
        var index: Int
    }

    /// The packs in the container. The renderer does not execute them yet; this
    /// is what the UI needs to be honest about that (see docs/SHADERS.md).
    var shaderPacks: [ShaderPack] {
        guard let raw else { return [] }
        let count = Int(hz_game_shader_count(raw))
        guard count > 0 else { return [] }
        var packs: [ShaderPack] = []
        packs.reserveCapacity(count)
        var nameBuffer = [CChar](repeating: 0, count: 128)
        var summaryBuffer = [CChar](repeating: 0, count: 320)
        for index in 0..<count {
            let i = UInt32(index)
            let nameLength = nameBuffer.withUnsafeMutableBufferPointer { ptr -> UInt32 in
                guard let base = ptr.baseAddress else { return 0 }
                return hz_game_shader_name(raw, i, base, UInt32(ptr.count))
            }
            let summaryLength = summaryBuffer.withUnsafeMutableBufferPointer { ptr -> UInt32 in
                guard let base = ptr.baseAddress else { return 0 }
                return hz_game_shader_summary(raw, i, base, UInt32(ptr.count))
            }
            packs.append(ShaderPack(
                id: nameLength > 0 ? String(cString: nameBuffer) : "shader#\(index)",
                summary: summaryLength > 0 ? String(cString: summaryBuffer) : "",
                supported: hz_game_shader_supported(raw, i) == 1,
                index: index
            ))
        }
        return packs
    }

    /// Translated Metal source for pack `index`, when the porter produced it.
    /// Returns nil for refused packs or packs without shaders.
    func shaderMSL(index: Int) -> String? {
        guard let raw else { return nil }
        let i = UInt32(index)
        let length = Int(hz_game_shader_msl_len(raw, i))
        guard length > 0 else { return nil }
        var bytes = [UInt8](repeating: 0, count: length)
        let written = bytes.withUnsafeMutableBufferPointer { ptr -> UInt32 in
            guard let base = ptr.baseAddress else { return 0 }
            return hz_game_shader_msl(raw, i, base, UInt32(ptr.count))
        }
        guard written > 0 else { return nil }
        return String(decoding: bytes.prefix(Int(written)), as: UTF8.self)
    }

    /// How many packs the engine could drive as-is.
    var supportedShaderCount: Int {
        guard let raw else { return 0 }
        return Int(hz_game_supported_shader_count(raw))
    }

    var airID: UInt32 {
        guard let raw else { return 0 }
        return hz_game_air_id(raw)
    }

    // MARK: Textures

    struct TextureRef: Sendable {
        var id: String
        var png: Data
    }

    /// Every texture referenced by the pack, in engine order. The renderer
    /// uploads them as one `texture2d_array` so a quad's slot index addresses
    /// a slice directly.
    func textures() -> [TextureRef] {
        guard let raw else { return [] }
        let count = Int(hz_game_texture_count(raw))
        guard count > 0 else { return [] }
        var refs: [TextureRef] = []
        refs.reserveCapacity(count)
        var nameBuffer = [CChar](repeating: 0, count: 256)
        for i in 0..<count {
            let nameLength = nameBuffer.withUnsafeMutableBufferPointer { ptr -> UInt32 in
                guard let base = ptr.baseAddress else { return 0 }
                return hz_game_texture_name(raw, UInt32(i), base, UInt32(ptr.count))
            }
            let id = nameLength > 0
                ? String(cString: nameBuffer)
                : "texture#\(i)"
            let length = Int(hz_game_texture_png_len(raw, UInt32(i)))
            var png = Data()
            if length > 0 {
                var bytes = [UInt8](repeating: 0, count: length)
                let written = bytes.withUnsafeMutableBufferPointer { ptr -> UInt32 in
                    guard let base = ptr.baseAddress else { return 0 }
                    return hz_game_texture_png(raw, UInt32(i), base, UInt32(ptr.count))
                }
                if written > 0 {
                    png = Data(bytes.prefix(Int(written)))
                }
            }
            refs.append(TextureRef(id: id, png: png))
        }
        return refs
    }

    struct Stats: Sendable {
        var ticks: UInt32 = 0
        var frames: UInt32 = 0
        var quads: UInt32 = 0
        var nonAir: UInt32 = 0
    }

    var stats: Stats {
        guard let raw else { return Stats() }
        var out = [UInt32](repeating: 0, count: 4)
        let n = out.withUnsafeMutableBufferPointer { ptr in
            hz_game_stats(raw, ptr.baseAddress)
        }
        guard n == 4 else { return Stats() }
        return Stats(ticks: out[0], frames: out[1], quads: out[2], nonAir: out[3])
    }

    func save(to url: URL) throws {
        guard let raw else { throw EngineError.packRejected("会话已结束") }
        let status = url.path.withCString { hz_game_save(raw, $0) }
        if status != 0 { throw EngineError.portFailed(Engine.lastError()) }
    }

    func load(from url: URL) throws {
        guard let raw else { throw EngineError.packRejected("会话已结束") }
        let status = url.path.withCString { hz_game_load(raw, $0) }
        if status != 0 { throw EngineError.portFailed(Engine.lastError()) }
    }
}