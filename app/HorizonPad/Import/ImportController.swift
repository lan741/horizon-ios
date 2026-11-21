//
//  ImportController.swift
//  HorizonPad
//
//  The import flow, start to finish:
//    pick a .jar → copy into the container → port on the engine queue →
//    read the report → register in the library.
//
//  Import happens entirely on device. Nothing is uploaded, and the app has no
//  networking entitlement.
//

import Foundation
import SwiftUI
import UniformTypeIdentifiers

@MainActor
final class ImportController: ObservableObject {
    enum Phase: Equatable {
        case idle
        case copying
        case porting
        case done
        case failed(String)
    }

    @Published private(set) var phase: Phase = .idle
    @Published private(set) var progress = PortProgress()
    @Published var pendingURL: URL?
    @Published var minecraftVersion: String = ""
    @Published var displayName: String = ""

    /// Accept standard ZIP-shaped documents: a Minecraft client jar is a zip.
    static var allowedTypes: [UTType] {
        var types: [UTType] = [.zip, .archive]
        if let jar = UTType(filenameExtension: "jar") { types.append(jar) }
        if let mrpack = UTType(filenameExtension: "mrpack") { types.append(mrpack) }
        return types
    }

    func begin(url: URL) {
        pendingURL = url
        displayName = url.deletingPathExtension().lastPathComponent
        minecraftVersion = guessVersion(from: url) ?? ""
        phase = .idle
    }

    /// Best-effort: `1.21.4.jar`, `client-26.3.jar`, `minecraft-26.3-client`.
    private func guessVersion(from url: URL) -> String? {
        let name = url.deletingPathExtension().lastPathComponent
        let pattern = #"(\d{1,2}\.\d{1,2}(?:\.\d{1,2})?|[12]\d\.\d)"#
        guard let range = name.range(of: pattern, options: .regularExpression) else { return nil }
        return String(name[range])
    }

    func run() async {
        guard let url = pendingURL else { return }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }

        let id = UUID()
        let jarURL: URL
        do {
            phase = .copying
            jarURL = try Paths.importCopy(of: url, into: Paths.jars,
                                          preferredName: "\(id.uuidString).jar")
        } catch {
            phase = .failed("无法复制文件：\(error.localizedDescription)")
            return
        }

        let packURL = Paths.packURL(for: id)
        let reportURL = Paths.reportURL(for: id)
        let version = minecraftVersion.trimmingCharacters(in: .whitespaces).isEmpty
            ? "unknown"
            : minecraftVersion

        phase = .porting
        let outcome: PortOutcome
        do {
            outcome = try await withCheckedThrowingContinuation { continuation in
                Engine.queue.async {
                    do {
                        let result = try Porter.port(
                            jar: jarURL,
                            pack: packURL,
                            report: reportURL,
                            minecraftVersion: version
                        ) { progress in
                            Task { @MainActor in
                                ImportController.sharedProgress(progress)
                            }
                        }
                        continuation.resume(returning: result)
                    } catch {
                        continuation.resume(throwing: error)
                    }
                }
            }
        } catch {
            phase = .failed(error.localizedDescription)
            LogStore.shared.error("port failed: \(error.localizedDescription)")
            return
        }

        // The jar itself is not needed after porting; keeping it would double
        // the storage for a file we cannot use again on device.
        try? FileManager.default.removeItem(at: jarURL)

        let report = Self.parseReport(outcome.reportJSON)
        let pack = InstalledPack(
            id: id,
            displayName: displayName.isEmpty ? "Horizon 资源包" : displayName,
            minecraftVersion: version,
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
        LogStore.shared.info("ported \(pack.displayName): \(pack.entryCount) entries, coverage \(String(format: "%.1f", pack.coverage * 100))%")
        phase = .done
    }

    private static func sharedProgress(_ p: PortProgress) {
        ImportController.current?.progress = p
    }

    /// Set by the view so progress callbacks can reach the live controller.
    static weak var current: ImportController?

    struct ParsedReport {
        var coverage: Double = 0
        var javaClasses: Int = 0
        var jvm: JVMSummary?
    }

    static func parseReport(_ json: String) -> ParsedReport {
        guard
            let data = json.data(using: .utf8),
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return ParsedReport() }
        var parsed = ParsedReport()
        parsed.coverage = obj["coverage"] as? Double ?? 0
        parsed.javaClasses = obj["java_classes"] as? Int ?? 0
        if let jvm = obj["jvm"] as? [String: Any] {
            parsed.jvm = JVMSummary(json: jvm)
        }
        return parsed
    }

    func reset() {
        phase = .idle
        pendingURL = nil
        progress = PortProgress()
        displayName = ""
    }
}