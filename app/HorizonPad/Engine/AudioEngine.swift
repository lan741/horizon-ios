//
//  AudioEngine.swift
//  HorizonPad
//
//  Plays whatever the engine queues.
//
//  iOS's AVFoundation cannot decode Ogg Vorbis, and Minecraft ships sounds as
//  Ogg — so the engine carries the decoder instead: `stb_vorbis` is vendored and
//  compiled into the static library (see engine/crates/hz-vorbis). This file
//  turns the engine's PCM into an AVAudioPCMBuffer:
//
//    * Ogg payloads: `hz_pcm_decode` → interleaved f32 → buffer (the main path);
//    * anything else AVFoundation understands (WAV/AIFF/CAF/MP3/AAC): decoded
//      through AVAudioFile, so user-supplied sounds work too;
//    * if a build has no decoder (`hz_vorbis_available() == 0`), Ogg assets are
//      counted and reported — never silently dropped.
//

import AVFoundation
import Foundation
import os

@MainActor
final class AudioEngine {
    /// One decoded buffer per sound asset we can actually play.
    private var players: [UInt16: AVAudioPlayerNode] = [:]
    private var buffers: [UInt16: AVAudioPCMBuffer] = [:]
    private var unsupported: UInt16 = 0
    private var played: UInt32 = 0

    private let engine = AVAudioEngine()
    private let logger = Logger(subsystem: "dev.playhorizon.pad", category: "audio")
    private var started = false

    private(set) var loadedCount = 0
    private(set) var unsupportedCount = 0

    /// Load every asset the engine knows about. Ogg files land in
    /// `unsupportedCount`; everything else is decoded into a PCM buffer.
    func load(from handle: GameHandle) {
        start()
        let total = Int(hz_game_sound_asset_count(handle.raw))
        guard total > 0 else { return }

        for index in 0..<total {
            let i = UInt32(index)
            let length = Int(hz_game_sound_len(handle.raw, i))
            guard length > 0 else { continue }
            var bytes = [UInt8](repeating: 0, count: length)
            let written = bytes.withUnsafeMutableBufferPointer { ptr -> UInt32 in
                guard let base = ptr.baseAddress else { return 0 }
                return hz_game_sound_data(handle.raw, i, base, UInt32(ptr.count))
            }
            guard written > 0 else { continue }
            let data = Data(bytes.prefix(Int(written)))

            // Ask the engine what container it found before trying to decode.
            var info = [UInt32](repeating: 0, count: 4)
            let haveInfo = info.withUnsafeMutableBufferPointer { ptr -> UInt32 in
                guard let base = ptr.baseAddress else { return 0 }
                return hz_game_sound_info(handle.raw, i, base)
            }
            let isVorbis = haveInfo == 4 && info[0] > 0

            if isVorbis {
                if let buffer = decodeVorbis(data) {
                    let player = AVAudioPlayerNode()
                    engine.attach(player)
                    engine.connect(player, to: engine.mainMixerNode, format: buffer.format)
                    players[UInt16(index)] = player
                    buffers[UInt16(index)] = buffer
                    loadedCount += 1
                } else {
                    // Name it: "which sounds are silent" is the first question
                    // anyone asks when audio is missing.
                    let id = soundID(handle: handle, index: i)
                    logger.info("could not decode: \(id, privacy: .public)")
                    unsupported = nextUnsupported(unsupported)
                }
                continue
            }
            guard let buffer = decode(data) else {
                unsupported += 1
                unsupportedCount = Int(unsupported)
                continue
            }
            let player = AVAudioPlayerNode()
            engine.attach(player)
            engine.connect(player, to: engine.mainMixerNode, format: buffer.format)
            players[UInt16(index)] = player
            buffers[UInt16(index)] = buffer
            loadedCount += 1
        }

        if unsupportedCount > 0 {
            logger.info("\(self.unsupportedCount, privacy: .public) sound(s) need an Ogg decoder; \(self.loadedCount, privacy: .public) playable")
            Task { @MainActor in
                LogStore.shared.warn(
                    "\(self.unsupportedCount) 个音效是 Ogg Vorbis，iOS 无法解码；\(self.loadedCount) 个可播放。见 docs/SOUND.md"
                )
            }
        }
    }

    /// Drain the engine's queue and play what it asked for.
    func pump(from handle: GameHandle) {
        var out = [Float](repeating: 0, count: 3)
        // Bound the loop: a burst must not stall the frame.
        for _ in 0..<8 {
            let got = out.withUnsafeMutableBufferPointer { ptr -> UInt32 in
                guard let base = ptr.baseAddress else { return 0 }
                return hz_game_next_sound(handle.raw, base)
            }
            guard got == 1 else { return }
            let asset = UInt16(out[0])
            let volume = out[1]
            let pitch = out[2]
            play(asset: asset, volume: volume, pitch: pitch)
        }
    }

    /// Decode an Ogg Vorbis payload with the engine's vendored decoder.
    ///
    /// The engine hands back interleaved f32 samples; AVFoundation wants a
    /// buffer in the same shape, so this is a straight copy with no resampling.
    private func decodeVorbis(_ data: Data) -> AVAudioPCMBuffer? {
        guard hz_vorbis_available() == 1 else { return nil }
        let pcm: OpaquePointer? = data.withUnsafeBytes { raw -> OpaquePointer? in
            guard let base = raw.bindMemory(to: UInt8.self).baseAddress else { return nil }
            return hz_pcm_decode(base, UInt32(data.count))
        }
        guard let pcm else {
            logger.error("stb_vorbis refused a payload")
            return nil
        }
        defer { hz_pcm_free(pcm) }

        let channels = hz_pcm_channels(pcm)
        let rate = hz_pcm_rate(pcm)
        let frames = hz_pcm_frames(pcm)
        guard channels > 0, rate > 0, frames > 0,
              let format = AVAudioFormat(commonFormat: .pcmFormatFloat32,
                                         sampleRate: Double(rate),
                                         channels: AVAudioChannelCount(channels),
                                         interleaved: true),
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames),
              let channelData = buffer.floatChannelData
        else { return nil }

        buffer.frameLength = frames
        let total = UInt32(frames * channels)
        let written = hz_pcm_data(pcm, channelData[0], total)
        guard written == total else {
            logger.error("PCM copy short: \(written, privacy: .public)/\(total, privacy: .public)")
            return nil
        }
        return buffer
    }

    /// The engine's id for a sound asset, for diagnostics.
    private func soundID(handle: GameHandle, index: UInt32) -> String {
        var buffer = [CChar](repeating: 0, count: 160)
        let written = buffer.withUnsafeMutableBufferPointer { ptr -> UInt32 in
            guard let base = ptr.baseAddress else { return 0 }
            return hz_game_sound_asset_id(handle.raw, index, base, UInt32(ptr.count))
        }
        return written > 0 ? String(cString: buffer) : "sound#\(index)"
    }

    private func play(asset: UInt16, volume: Float, pitch: Float) {
        guard let player = players[asset], let buffer = buffers[asset] else {
            return // an asset we could not decode; counted at load time
        }
        player.volume = min(max(volume, 0), 1)
        // `rate` is the only pitch control AVAudioPlayerNode offers without a
        // custom time-pitch unit; it is close enough for one-shot effects.
        player.rate = min(max(pitch, 0.5), 2.0)
        player.scheduleBuffer(buffer, at: nil, options: .interrupts, completionHandler: nil)
        if !player.isPlaying { player.play() }
        played &+= 1
    }

    private func decode(_ data: Data) -> AVAudioPCMBuffer? {
        // Write to a temporary file because AVAudioFile has no in-memory
        // initialiser; the file is removed immediately after opening.
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("hz-sound-\(UUID().uuidString)")
        do {
            try data.write(to: url)
            defer { try? FileManager.default.removeItem(at: url) }
            let file = try AVAudioFile(forReading: url)
            let frames = AVAudioFrameCount(file.length)
            guard frames > 0,
                  let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat,
                                                frameCapacity: frames) else { return nil }
            try file.read(into: buffer)
            return buffer
        } catch {
            return nil
        }
    }

    private func nextUnsupported(_ current: UInt16) -> UInt16 {
        unsupportedCount = Int(current) + 1
        return current &+ 1
    }

    private func start() {
        guard !started else { return }
        do {
            engine.prepare()
            try engine.start()
            started = true
        } catch {
            logger.error("audio engine failed to start: \(error.localizedDescription, privacy: .public)")
        }
    }

    func stop() {
        engine.stop()
        started = false
    }

    var stats: (loaded: Int, unsupported: Int, played: UInt32) {
        (loadedCount, unsupportedCount, played)
    }
}