//
//  GameSession.swift
//  HorizonPad
//
//  Runs the simulation clock and feeds the renderer. One session per play
//  screen; `stop()` is idempotent.
//

import Foundation
import QuartzCore
import simd

@MainActor
final class GameSession {
    private(set) var handle: GameHandle?
    private var timer: Timer?
    private var lastTick: CFTimeInterval = 0
    private var accumulator: Double = 0
    /// 20 Hz simulation, same as Minecraft's tick rate.
    private let tickInterval: Double = 1.0 / 20.0
    private let maxCatchUp: Double = 0.25

    private(set) var stats = GameHandle.Stats()
    private(set) var lastError: String? = nil

    private(set) var renderer: Renderer? = nil
    /// Playback side of the sound queue.
    let audio = AudioEngine()

    /// Movement input in −1…1, set by the touch controls.
    var moveAxis: SIMD2<Float> = .zero
    var lookDelta: SIMD2<Float> = .zero
    var flying = false

    private var yaw: Float = 0
    private var pitch: Float = 0
    private var position: SIMD3<Float> = SIMD3(0.5, 3, 0.5)

    init?(packURL: URL, device: MTLDevice?) {
        guard let device, let renderer = Renderer.make(device: device) else { return nil }
        self.renderer = renderer
        do {
            let handle = try GameHandle(packURL: packURL)
            self.handle = handle
            // Textures first: the vertex stream already carries slice indices.
            renderer.upload(textures: handle.textures())
            audio.load(from: handle)
            // Then adopt a shader pack if one was translated and compiles. A
            // failure leaves the built-in pipeline in place, which is the whole
            // point of the fallback.
            applyShaderPackIfAvailable(handle: handle, renderer: renderer)
            // Sync the engine's player to the camera so the first frame is sane.
            position = handle.playerPosition
            handle.rebuildMeshes()
            renderer.syncSections(handle.visibleSections(), from: handle)
            if handle.rebuildFrame() {
                renderer.submit(vertices: handle.frameVertices())
            }
        } catch {
            lastError = error.localizedDescription
            return nil
        }
    }

    /// Try the first pack that has translated source. Silent on success beyond a
    /// log line; on failure the built-in pipeline keeps drawing.
    private func applyShaderPackIfAvailable(handle: GameHandle, renderer: Renderer) {
        for pack in handle.shaderPacks where pack.supported {
            guard let msl = handle.shaderMSL(index: pack.index) else { continue }
            if renderer.applyShaderPack(name: pack.id, source: msl) {
                return
            }
        }
    }

    /// Turn the pack pipeline off (settings toggle, or a pack that misbehaves).
    func disableShaderPack() {
        renderer?.clearShaderPack()
    }

    func start() {
        guard timer == nil else { return }
        lastTick = CACurrentMediaTime()
        let t = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.step() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        audio.stop()
    }

    /// Break a block and ask for the matching sound, if the pack has one.
    @discardableResult
    func breakWithSound() -> Bool {
        guard let hit = crosshairHit else { return false }
        let broke = breakTargeted()
        if broke {
            let centre = SIMD3<Float>(Float(hit.block.x) + 0.5,
                                      Float(hit.block.y) + 0.5,
                                      Float(hit.block.z) + 0.5)
            handle?.play(event: "block.stone.break", at: centre)
        }
        return broke
    }

    private func step() {
        guard let handle, let renderer else { return }
        let now = CACurrentMediaTime()
        let frameTime = min(now - lastTick, maxCatchUp)
        lastTick = now
        accumulator += frameTime

        // Apply look input (radians per point of drag).
        if lookDelta != .zero {
            yaw -= lookDelta.x * 0.004
            pitch = min(max(pitch - lookDelta.y * 0.004, -1.55), 1.55)
            lookDelta = .zero
        }

        var ticksRun = 0
        while accumulator >= tickInterval {
            accumulator -= tickInterval
            ticksRun += 1
            if ticksRun > 8 { break } // never spiral

            // Movement is expressed in the camera's yaw frame; the engine
            // integrates gravity and collisions.
            let forward = SIMD3<Float>(-sin(yaw), 0, -cos(yaw))
            let right = SIMD3<Float>(forward.z, 0, -forward.x)
            let wish = forward * moveAxis.y + right * moveAxis.x
            if simd_length(wish) > 0.001 {
                let step = simd_normalize(wish) * 4.3 * Float(tickInterval)
                position += SIMD3(step.x, 0, step.z)
            }
            if flying {
                // Temporary: vertical control from the look pitch while flying.
                position.y += pitch * 2.0 * Float(tickInterval)
            }
            handle.setPlayer(x: position.x, y: position.y, z: position.z, yaw: yaw, pitch: pitch)
            handle.tick(Float(tickInterval))
            position = handle.playerPosition
        }

        if ticksRun > 0 {
            // Per-section path: rebuild without assembling, then let the renderer
            // re-upload only the sections whose generation changed.
            if handle.rebuildMeshes() != 0 {
                renderer.syncSections(handle.visibleSections(), from: handle)
                // Keep the flat buffer current as a fallback for the first frame
                // and for anything that still reads it (benchmarks, screenshots).
                if handle.rebuildFrame() {
                    renderer.submit(vertices: handle.frameVertices())
                }
            }
            audio.pump(from: handle)
            stats = handle.stats
        }

        renderer.camera.position = position + SIMD3(0, 1.62, 0)
        renderer.camera.yaw = yaw
        renderer.camera.pitch = pitch
        renderer.camera.time = Float(now)
    }

    /// Daylight 0…1, driving the sky channel.
    var daylight: Float {
        get { handle?.daylight ?? 1 }
        set {
            handle?.daylight = newValue
            // Safe to call every frame: it returns false when nothing changed,
            // and re-baking happens once for the whole world.
            if let handle, handle.rebuildFrame() {
                renderer?.submit(vertices: handle.frameVertices())
            }
        }
    }

    /// Light under the crosshair, for the debug HUD.
    var crosshairLight: GameHandle.LightSample? {
        guard let handle, let hit = crosshairHit else { return nil }
        return handle.light(at: hit.block)
    }

    /// What "place" should put down: stone when the pack has it, otherwise the
    /// first non-air block.
    func suggestedPlaceBlock() -> UInt32 {
        guard let handle else { return 0 }
        if let stone = handle.blockID("minecraft:stone") {
            return stone
        }
        if handle.blockCount > 1 {
            return 1
        }
        return 0
    }

    /// The block under the crosshair, if any, within reach.
    var crosshairHit: GameHandle.RayHit? {
        guard let handle else { return nil }
        let eye = position + SIMD3(0, 1.62, 0)
        let direction = SIMD3<Float>(-sin(yaw) * cos(pitch), sin(pitch), -cos(yaw) * cos(pitch))
        return handle.raycast(origin: eye, direction: direction, maxDistance: 6)
    }

    /// Break the targeted block. Returns true when something was removed.
    @discardableResult
    func breakTargeted() -> Bool {
        guard let handle, let hit = crosshairHit else { return false }
        handle.setBlock(x: hit.block.x, y: hit.block.y, z: hit.block.z, id: handle.airID)
        refreshAfterEdit()
        Haptics.tap()
        return true
    }

    /// Place `blockID` against the targeted face.
    @discardableResult
    func placeTargeted(blockID: UInt32) -> Bool {
        guard let handle, let hit = crosshairHit else { return false }
        let target = hit.adjacent
        // The engine refuses placements that would overlap the player; mirror
        // that check here only to report it, then let the engine decide.
        handle.setBlock(x: target.x, y: target.y, z: target.z, id: blockID)
        refreshAfterEdit()
        Haptics.tap()
        return true
    }

    private func refreshAfterEdit() {
        guard let handle else { return }
        if handle.rebuildMeshes() != 0 {
            renderer?.syncSections(handle.visibleSections(), from: handle)
        }
        stats = handle.stats
    }

    func teleport(spawn: Bool) {
        guard let handle else { return }
        position = SIMD3(0.5, spawn ? 4 : position.y, 0.5)
        handle.setPlayer(x: position.x, y: position.y, z: position.z, yaw: yaw, pitch: pitch)
    }

    func save(to url: URL) {
        do { try handle?.save(to: url) } catch { lastError = error.localizedDescription }
    }

    func load(from url: URL) {
        do {
            try handle?.load(from: url)
            if let handle {
                position = handle.playerPosition
                if handle.rebuildFrame() {
                    renderer?.submit(vertices: handle.frameVertices())
                }
            }
        } catch {
            lastError = error.localizedDescription
        }
    }
}