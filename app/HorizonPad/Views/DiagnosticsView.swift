//
//  DiagnosticsView.swift
//  HorizonPad
//
//  Everything a bug report needs, on one screen, with nothing sent anywhere.
//

import SwiftUI

/// One environment key/value line, for the same reason as `KindRow`.
struct EnvRow: Identifiable {
    let id: String
    let key: String
    let value: String

    static func rows(from environment: [String: String]) -> [EnvRow] {
        environment
            .sorted { $0.key < $1.key }
            .map { EnvRow(id: $0.key, key: $0.key, value: $0.value) }
    }
}

struct DiagnosticsView: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var library: Library
    @ObservedObject private var log = LogStore.shared
    @State private var exported: URL?

    var body: some View {
        Form {
            Section(header: Text("环境")) {
                ForEach(EnvRow.rows(from: model.environment())) { row in
                    LabeledContent(row.key, value: row.value)
                }
            }

            Section(header: Text("资源包")) {
                if library.packs.isEmpty {
                    Text("无").foregroundStyle(.secondary)
                }
                ForEach(library.packs) { pack in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(pack.displayName)
                        Text("\(pack.entryCount) 项 · \(String(format: "%.1f", pack.coverage * 100))% · MC \(pack.minecraftVersion)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        if let jvm = pack.jvmSummary {
                            Text("Java 残留：\(jvm.classes) 类 / \(jvm.methods) 方法")
                                .font(.caption2)
                                .foregroundStyle(.tertiary)
                        }
                    }
                }
            }

            Section {
                Button {
                    export()
                } label: {
                    Label("导出诊断日志", systemImage: "square.and.arrow.up")
                }
                if let exported {
                    ShareLink(item: exported) {
                        Label("分享 \(exported.lastPathComponent)", systemImage: "doc.text")
                    }
                }
                Button("清空日志", role: .destructive) { log.clear() }
            } footer: {
                Text("日志只保存在本机，导出由你决定。")
            }

            Section(header: Text("日志")) {
                ForEach(log.lines.suffix(120)) { line in
                    Text(line.text)
                        .font(.caption2.monospaced())
                        .foregroundStyle(color(for: line.level))
                }
            }
        }
        .navigationTitle("诊断")
    }

    private func color(for level: LogLevel) -> Color {
        switch level {
        case .debug: .secondary
        case .info: .primary
        case .warning: .orange
        case .error: .red
        }
    }

    private func export() {
        let text = log.exportText(environment: model.environment())
        let url = Paths.root.appendingPathComponent("diagnostics.txt")
        do {
            try text.write(to: url, atomically: true, encoding: .utf8)
            exported = url
        } catch {
            model.lastError = error.localizedDescription
        }
    }
}