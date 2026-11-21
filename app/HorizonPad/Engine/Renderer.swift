//
//  Renderer.swift
//  HorizonPad
//
//  Metal renderer for the engine's interleaved vertex stream.
//
//  Vertex layout (16 × Float), matching hz_game::frame:
//    position.xyz | uv.uv | normal.xyz | color.rgba | texture | v_delta | blend | pad
//
//  Textures arrive as raw PNG bytes from the engine and are uploaded as a
//  single `texture2d_array`; a vertex's `texture` field is the array slice.
//  Nearest filtering on purpose: block textures are pixel art.
//

import Metal
import MetalKit
import simd

struct CameraUniforms {
    var viewProjection: simd_float4x4
    var cameraPosition: SIMD4<Float>
    var fogColor: SIMD4<Float>
    var params: SIMD4<Float> // x fog start, y fog end, z time, w texture count
}

/// Layout must match the `HzUniforms` struct the translator emits:
/// `float4x4 mvp; float4 cameraPosition; float4 fogColor; float4 fogParams;
/// float time; float daylight;`
struct ShaderUniforms {
    var mvp: simd_float4x4
    var cameraPosition: SIMD4<Float>
    var fogColor: SIMD4<Float>
    var fogParams: SIMD4<Float>
    var time: Float
    var daylight: Float
}

final class Renderer: NSObject, MTKViewDelegate {
    private let device: MTLDevice
    private let queue: MTLCommandQueue
    private let pipeline: MTLRenderPipelineState
    private let depthState: MTLDepthStencilState
    private let sampler: MTLSamplerState

    private var vertexBuffer: MTLBuffer
    private var vertexCapacityFloats: Int
    private var vertexCount: Int = 0

    private(set) var camera = Camera()
    private var uniformBuffer: MTLBuffer

    /// One array slice per engine texture; nil until `upload(textures:)` runs.
    private var textureArray: MTLTexture?
    private var textureCount: Int = 0

    /// Pipeline built from a pack's translated Metal source, when one compiled.
    private(set) var packPipeline: MTLRenderPipelineState?
    private(set) var packPipelineName: String?
    private var packUniformBuffer: MTLBuffer
    /// Set when a pack pipeline is active: draws use it instead of the built-in.
    private(set) var usingPackPipeline = false

    /// One GPU buffer per section, keyed by "x,y,z". Only re-uploaded when the
    /// engine's generation stamp for that section changes.
    private struct SectionBuffer {
        var buffer: MTLBuffer
        var capacityFloats: Int
        var vertexCount: Int
        var generation: UInt32
    }
    private var sectionBuffers: [String: SectionBuffer] = [:]
    /// Draw order for the current frame.
    private var drawList: [MTLBuffer] = []
    private var drawVertexCounts: [Int] = []
    /// Bytes uploaded since the last reset, for the diagnostics screen.
    private(set) var uploadedBytes: Int = 0

    /// Vertex data is produced on the engine queue and handed over here.
    private var pendingVertices: [Float] = []
    private let lock = NSLock()

    /// Build everything first, then hand it to a non-failable initialiser.
    /// A class's failable `init` may not return nil before every stored
    /// property is initialised, so the factory owns the guards.
    static func make(device: MTLDevice) -> Renderer? {
        guard
            let queue = device.makeCommandQueue(),
            let library = device.makeDefaultLibrary(),
            let vertexFn = library.makeFunction(name: "hz_vertex"),
            let fragmentFn = library.makeFunction(name: "hz_fragment")
        else { return nil }

        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.vertexFunction = vertexFn
        descriptor.fragmentFunction = fragmentFn
        descriptor.colorAttachments[0].pixelFormat = .bgra8Unorm
        descriptor.depthAttachmentPixelFormat = .depth32Float

        let vd = MTLVertexDescriptor()
        vd.attributes[0].format = .float3 // position
        vd.attributes[0].offset = 0
        vd.attributes[0].bufferIndex = 0
        vd.attributes[1].format = .float2 // uv
        vd.attributes[1].offset = MemoryLayout<Float>.stride * 3
        vd.attributes[1].bufferIndex = 0
        vd.attributes[2].format = .float3 // normal
        vd.attributes[2].offset = MemoryLayout<Float>.stride * 5
        vd.attributes[2].bufferIndex = 0
        vd.attributes[3].format = .float4 // color
        vd.attributes[3].offset = MemoryLayout<Float>.stride * 8
        vd.attributes[3].bufferIndex = 0
        vd.attributes[4].format = .float // texture slice
        vd.attributes[4].offset = MemoryLayout<Float>.stride * 12
        vd.attributes[4].bufferIndex = 0
        vd.attributes[5].format = .float // v_delta for animated textures
        vd.attributes[5].offset = MemoryLayout<Float>.stride * 13
        vd.attributes[5].bufferIndex = 0
        vd.attributes[6].format = .float // blend between current and next frame
        vd.attributes[6].offset = MemoryLayout<Float>.stride * 14
        vd.attributes[6].bufferIndex = 0
        vd.layouts[0].stride = MemoryLayout<Float>.stride * 16
        descriptor.vertexDescriptor = vd

        guard let pipeline = try? device.makeRenderPipelineState(descriptor: descriptor) else {
            return nil
        }

        let ds = MTLDepthStencilDescriptor()
        ds.depthCompareFunction = .less
        ds.isDepthWriteEnabled = true
        guard let depthState = device.makeDepthStencilState(descriptor: ds) else { return nil }

        let sd = MTLSamplerDescriptor()
        sd.minFilter = .nearest
        sd.magFilter = .nearest
        sd.mipFilter = .nearest
        sd.sAddressMode = .clampToEdge
        sd.tAddressMode = .clampToEdge
        guard let sampler = device.makeSamplerState(descriptor: sd) else { return nil }

        let initialFloats = 6 * 16 * 4096
        guard let buffer = device.makeBuffer(length: initialFloats * MemoryLayout<Float>.stride,
                                             options: .storageModeShared) else { return nil }

        guard let uniforms = device.makeBuffer(length: MemoryLayout<CameraUniforms>.stride,
                                               options: .storageModeShared) else { return nil }
        guard let packUniforms = device.makeBuffer(length: MemoryLayout<ShaderUniforms>.stride,
                                                   options: .storageModeShared) else { return nil }

        return Renderer(device: device,
                        queue: queue,
                        pipeline: pipeline,
                        depthState: depthState,
                        sampler: sampler,
                        vertexBuffer: buffer,
                        vertexCapacityFloats: initialFloats,
                        uniformBuffer: uniforms,
                        packUniformBuffer: packUniforms)
    }

    private init(device: MTLDevice,
                 queue: MTLCommandQueue,
                 pipeline: MTLRenderPipelineState,
                 depthState: MTLDepthStencilState,
                 sampler: MTLSamplerState,
                 vertexBuffer: MTLBuffer,
                 vertexCapacityFloats: Int,
                 uniformBuffer: MTLBuffer,
                 packUniformBuffer: MTLBuffer) {
        self.device = device
        self.queue = queue
        self.pipeline = pipeline
        self.depthState = depthState
        self.sampler = sampler
        self.vertexBuffer = vertexBuffer
        self.vertexCapacityFloats = vertexCapacityFloats
        self.uniformBuffer = uniformBuffer
        self.packUniformBuffer = packUniformBuffer
        super.init()
    }

    // MARK: Textures

    /// Decode every PNG and pack them into one array texture.
    ///
    /// `MTKTextureLoader` handles PNG; slices are copied with a blit so the
    /// array is one allocation and the shader can index it directly. Textures
    /// that fail to decode are replaced by a 1×1 magenta slice, which makes
    /// missing assets obvious instead of silently black.
    /// Compile a pack's Metal source and adopt it as the drawing pipeline.
    ///
    /// Failure is not fatal: the built-in pipeline stays active and the reason is
    /// logged. A pack that translates but does not compile must render *something*
    /// rather than a black screen.
    @discardableResult
    func applyShaderPack(name: String, source: String) -> Bool {
        let options = MTLCompileOptions()
        options.fastMathEnabled = true
        let library: MTLLibrary
        do {
            library = try device.makeLibrary(source: source, options: options)
        } catch {
            let message = "shader pack \(name) did not compile: \(error.localizedDescription)"
            Task { @MainActor in LogStore.shared.warn(message) }
            return false
        }
        guard
            let vertexFn = library.makeFunction(name: "hz_vertex"),
            let fragmentFn = library.makeFunction(name: "hz_fragment")
        else {
            Task { @MainActor in
                LogStore.shared.warn("shader pack \(name): hz_vertex/hz_fragment missing")
            }
            return false
        }

        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.vertexFunction = vertexFn
        descriptor.fragmentFunction = fragmentFn
        descriptor.colorAttachments[0].pixelFormat = .bgra8Unorm
        descriptor.depthAttachmentPixelFormat = .depth32Float

        // The pack declares its inputs by name; the engine's buffer offsets are
        // fixed (see hz-port's ENGINE_ATTRIBUTES), so the descriptor mirrors them.
        let vd = MTLVertexDescriptor()
        let layout: [(Int, MTLVertexFormat, Int)] = [
            (0, .float3, 0),   // position
            (1, .float2, 3),   // uv
            (2, .float3, 5),   // normal
            (3, .float4, 8),   // color
            (4, .float, 12),   // texture index
        ]
        for (slot, format, floatOffset) in layout {
            vd.attributes[slot].format = format
            vd.attributes[slot].offset = MemoryLayout<Float>.stride * floatOffset
            vd.attributes[slot].bufferIndex = 0
        }
        vd.layouts[0].stride = MemoryLayout<Float>.stride * 16
        descriptor.vertexDescriptor = vd

        do {
            let state = try device.makeRenderPipelineState(descriptor: descriptor)
            packPipeline = state
            packPipelineName = name
            usingPackPipeline = true
            Task { @MainActor in LogStore.shared.info("shader pack \(name) is driving the renderer") }
            return true
        } catch {
            Task { @MainActor in
                LogStore.shared.warn("shader pack \(name): pipeline creation failed: \(error.localizedDescription)")
            }
            return false
        }
    }

    /// Go back to the built-in pipeline.
    func clearShaderPack() {
        packPipeline = nil
        packPipelineName = nil
        usingPackPipeline = false
    }

    @discardableResult
    func upload(textures: [GameHandle.TextureRef]) -> Int {
        guard !textures.isEmpty else {
            textureArray = nil
            textureCount = 0
            return 0
        }
        let loader = MTKTextureLoader(device: device)
        let options: [MTKTextureLoader.Option: Any] = [
            .SRGB: NSNumber(value: false),
            .generateMipmaps: NSNumber(value: false),
            .textureUsage: NSNumber(value: MTLTextureUsage.shaderRead.rawValue),
            .textureStorageMode: NSNumber(value: MTLStorageMode.private.rawValue),
        ]

        // Decode first so the array can be sized to the widest/tallest texture.
        var decoded: [(MTLTexture?, String)] = []
        var maxWidth = 1
        var maxHeight = 1
        for ref in textures {
            var texture: MTLTexture?
            if !ref.png.isEmpty {
                texture = try? loader.newTexture(data: ref.png, options: options)
            }
            if let texture {
                maxWidth = max(maxWidth, texture.width)
                maxHeight = max(maxHeight, texture.height)
            }
            decoded.append((texture, ref.id))
        }

        // Non-square or oversized textures cannot share a 2D array slice size,
        // so the array is built at the largest size and UV scaling is baked by
        // nearest sampling (block textures are all 16×16 in practice).
        let descriptor = MTLTextureDescriptor()
        descriptor.textureType = .type2DArray
        descriptor.pixelFormat = .rgba8Unorm
        descriptor.width = maxWidth
        descriptor.height = maxHeight
        descriptor.arrayLength = decoded.count
        descriptor.mipmapLevelCount = 1
        descriptor.usage = [.shaderRead]
        descriptor.storageMode = .private
        guard let array = device.makeTexture(descriptor: descriptor) else { return 0 }

        guard
            let scratch = device.makeTexture(descriptor: {
                let d = MTLTextureDescriptor()
                d.textureType = .type2D
                d.pixelFormat = .rgba8Unorm
                d.width = maxWidth
                d.height = maxHeight
                d.usage = [.shaderRead]
                d.storageMode = .shared
                return d
            }()),
            let command = queue.makeCommandBuffer(),
            let blit = command.makeBlitCommandEncoder()
        else { return 0 }

        // Clear the scratch to magenta, then copy in each decoded slice.
        let pixels = [UInt8](repeating: 0, count: maxWidth * maxHeight * 4)
        var magenta = pixels
        for i in stride(from: 0, to: magenta.count, by: 4) {
            magenta[i] = 255
            magenta[i + 1] = 0
            magenta[i + 2] = 255
            magenta[i + 3] = 255
        }

        for (slice, entry) in decoded.enumerated() {
            if let texture = entry.0, texture.width == maxWidth, texture.height == maxHeight {
                blit.copy(from: texture,
                          sourceSlice: 0,
                          sourceLevel: 0,
                          sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
                          sourceSize: MTLSize(width: maxWidth, height: maxHeight, depth: 1),
                          to: array,
                          destinationSlice: slice,
                          destinationLevel: 0,
                          destinationOrigin: MTLOrigin(x: 0, y: 0, z: 0))
            } else {
                // Fill this slice with the fallback colour, then mark it.
                magenta.withUnsafeBytes { raw in
                    scratch.replace(region: MTLRegionMake2D(0, 0, maxWidth, maxHeight),
                                    mipmapLevel: 0,
                                    withBytes: raw.baseAddress!,
                                    bytesPerRow: maxWidth * 4)
                }
                blit.copy(from: scratch,
                          sourceSlice: 0,
                          sourceLevel: 0,
                          sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
                          sourceSize: MTLSize(width: maxWidth, height: maxHeight, depth: 1),
                          to: array,
                          destinationSlice: slice,
                          destinationLevel: 0,
                          destinationOrigin: MTLOrigin(x: 0, y: 0, z: 0))
            }
        }
        blit.endEncoding()
        command.commit()
        command.waitUntilCompleted()

        textureArray = array
        textureCount = decoded.count
        let missing = decoded.filter { $0.0 == nil }.count
        let summary = "textures: \(decoded.count) slices, \(missing) missing, \(maxWidth)×\(maxHeight)"
        Task { @MainActor in LogStore.shared.info(summary) }
        return decoded.count
    }

    var uploadedTextureCount: Int { textureCount }

    // MARK: Per-section geometry

    /// Synchronise the GPU buffers with the engine's sections.
    ///
    /// Only sections whose generation moved are copied; everything else keeps
    /// its buffer from the previous frame. This is the whole point of the
    /// per-section path: an edit uploads kilobytes, not the whole world.
    func syncSections(_ sections: [GameHandle.Section], from handle: GameHandle) {
        var live = Set<String>()
        drawList.removeAll(keepingCapacity: true)
        drawVertexCounts.removeAll(keepingCapacity: true)

        for section in sections {
            live.insert(section.key)
            var entry = sectionBuffers[section.key]
            if entry == nil || entry!.generation != section.generation
                || entry!.vertexCount != section.vertexCount {
                guard let vertices = handle.sectionVertices(section) else { continue }
                let floats = vertices.count
                var buffer = entry?.buffer
                var capacity = entry?.capacityFloats ?? 0
                if buffer == nil || capacity < floats {
                    let grown = max(floats, 4096)
                    guard let made = device.makeBuffer(length: grown * MemoryLayout<Float>.stride,
                                                       options: .storageModeShared) else { continue }
                    buffer = made
                    capacity = grown
                }
                guard let buffer else { continue }
                buffer.contents().copyMemory(from: vertices,
                                             byteCount: floats * MemoryLayout<Float>.stride)
                uploadedBytes += floats * MemoryLayout<Float>.stride
                entry = SectionBuffer(buffer: buffer,
                                      capacityFloats: capacity,
                                      vertexCount: section.vertexCount,
                                      generation: section.generation)
                sectionBuffers[section.key] = entry
            }
            if let entry, entry.vertexCount > 0 {
                drawList.append(entry.buffer)
                drawVertexCounts.append(entry.vertexCount)
            }
        }

        // Drop buffers for sections that left the view: they cost nothing to
        // rebuild and holding them would grow without bound as the player moves.
        for key in sectionBuffers.keys where !live.contains(key) {
            sectionBuffers.removeValue(forKey: key)
        }

        if drawnFromFlat {
            drawnFromFlat = false
        }
    }

    /// True when the last draw used the flat buffer (fallback path).
    private(set) var drawnFromFlat = true

    // MARK: Frame data

    /// Called from the engine queue with a fresh vertex array.
    func submit(vertices: [Float]) {
        lock.lock()
        pendingVertices = vertices
        lock.unlock()
    }

    private func uploadIfNeeded() {
        lock.lock()
        let incoming = pendingVertices
        pendingVertices = []
        lock.unlock()
        guard !incoming.isEmpty else { return }

        if incoming.count > vertexCapacityFloats {
            let grown = max(incoming.count, vertexCapacityFloats * 2)
            if let buffer = device.makeBuffer(length: grown * MemoryLayout<Float>.stride,
                                              options: .storageModeShared) {
                vertexBuffer = buffer
                vertexCapacityFloats = grown
            }
        }
        vertexCount = min(incoming.count, vertexCapacityFloats)
        vertexBuffer.contents()
            .copyMemory(from: incoming, byteCount: vertexCount * MemoryLayout<Float>.stride)
    }

    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {
        camera.aspect = Float(max(1, size.width) / max(1, size.height))
    }

    func draw(in view: MTKView) {
        uploadIfNeeded()

        guard
            let pass = view.currentRenderPassDescriptor,
            let drawable = view.currentDrawable,
            let command = queue.makeCommandBuffer(),
            let encoder = command.makeRenderCommandEncoder(descriptor: pass)
        else { return }

        encoder.setDepthStencilState(depthState)
        encoder.setCullMode(.back)
        encoder.setFrontFacing(.counterClockwise)

        if vertexCount > 0 || !drawList.isEmpty {
            let activePipeline = usingPackPipeline ? packPipeline : nil
            if let activePipeline {
                // The pack's `HzUniforms`: mvp, camera, fog, then time/daylight.
                var uniforms = ShaderUniforms(
                    mvp: camera.viewProjectionMatrix,
                    cameraPosition: SIMD4(camera.position, 1),
                    fogColor: SIMD4(0.62, 0.74, 0.92, 1),
                    fogParams: SIMD4(camera.fogStart, camera.fogEnd, camera.time, 0),
                    time: camera.time,
                    daylight: camera.daylight
                )
                encoder.setVertexBytes(&uniforms,
                                       length: MemoryLayout<ShaderUniforms>.stride,
                                       index: 1)
                encoder.setFragmentBytes(&uniforms,
                                         length: MemoryLayout<ShaderUniforms>.stride,
                                         index: 1)
                encoder.setRenderPipelineState(activePipeline)
            } else {
                var uniforms = CameraUniforms(
                    viewProjection: camera.viewProjectionMatrix,
                    cameraPosition: SIMD4(camera.position, 1),
                    fogColor: SIMD4(0.62, 0.74, 0.92, 1),
                    params: SIMD4(camera.fogStart, camera.fogEnd, camera.time, Float(textureCount))
                )
                encoder.setVertexBytes(&uniforms,
                                       length: MemoryLayout<CameraUniforms>.stride,
                                       index: 1)
                encoder.setFragmentBytes(&uniforms,
                                         length: MemoryLayout<CameraUniforms>.stride,
                                         index: 1)
                encoder.setRenderPipelineState(pipeline)
            }
            encoder.setFragmentSamplerState(sampler, index: 0)
            if let textureArray {
                encoder.setFragmentTexture(textureArray, index: 0)
            }
            if drawList.isEmpty {
                // Fallback: the flat buffer, used before the first section sync
                // and by the benchmark harness.
                encoder.setVertexBuffer(vertexBuffer, offset: 0, index: 0)
                encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: vertexCount)
            } else {
                for (index, buffer) in drawList.enumerated() {
                    encoder.setVertexBuffer(buffer, offset: 0, index: 0)
                    encoder.drawPrimitives(type: .triangle,
                                           vertexStart: 0,
                                           vertexCount: drawVertexCounts[index])
                }
            }
        }

        encoder.endEncoding()
        command.present(drawable)
        command.commit()
    }
}

// MARK: - Camera

struct Camera {
    var position: SIMD3<Float> = .init(0, 3, 6)
    var yaw: Float = 0
    var pitch: Float = -0.2
    var fovY: Float = 70 * .pi / 180
    var near: Float = 0.05
    var far: Float = 512
    var aspect: Float = 4.0 / 3.0
    var fogStart: Float = 48
    var fogEnd: Float = 160
    var time: Float = 0
    /// Mirrors the game's daylight factor; packs get it as a uniform.
    var daylight: Float = 1

    /// Look direction from yaw/pitch, matching the engine's convention where
    /// yaw 0 faces −Z.
    var forward: SIMD3<Float> {
        let cy = cos(yaw), sy = sin(yaw)
        let cp = cos(pitch), sp = sin(pitch)
        return simd_normalize(SIMD3(-sy * cp, sp, -cy * cp))
    }

    var right: SIMD3<Float> {
        simd_normalize(simd_cross(forward, SIMD3(0, 1, 0)))
    }

    var viewProjectionMatrix: simd_float4x4 {
        let eye = position
        let center = position + forward
        let view = simd_float4x4.lookAt(eye: eye, center: center, up: SIMD3(0, 1, 0))
        let proj = simd_float4x4.perspective(fovY: fovY, aspect: aspect, near: near, far: far)
        return proj * view
    }
}

extension simd_float4x4 {
    static func perspective(fovY: Float, aspect: Float, near: Float, far: Float) -> simd_float4x4 {
        let y = 1 / tan(fovY * 0.5)
        let x = y / max(aspect, 0.0001)
        let z = far / (near - far)
        return simd_float4x4(columns: (
            SIMD4(x, 0, 0, 0),
            SIMD4(0, y, 0, 0),
            SIMD4(0, 0, z, -1),
            SIMD4(0, 0, z * near, 0)
        ))
    }

    static func lookAt(eye: SIMD3<Float>, center: SIMD3<Float>, up: SIMD3<Float>) -> simd_float4x4 {
        let f = simd_normalize(center - eye)
        let s = simd_normalize(simd_cross(f, up))
        let u = simd_cross(s, f)
        return simd_float4x4(columns: (
            SIMD4(s.x, u.x, -f.x, 0),
            SIMD4(s.y, u.y, -f.y, 0),
            SIMD4(s.z, u.z, -f.z, 0),
            SIMD4(-simd_dot(s, eye), -simd_dot(u, eye), simd_dot(f, eye), 1)
        ))
    }
}