//
//  PackDetailView.swift
//  HorizonPad
//

import SwiftUI

/// One "kind: count" line. A dedicated type keeps `ForEach` away from key
/// paths into dictionary tuples, which Swift does not support.
struct KindRow: Identifiable {
    let id: String
    let kind: String
    let count: Int

    static func rows(from kinds: [String: Int]) -> [KindRow] {
        kinds
            .filter { $0.value > 0 }
            .sorted { $0.key < $1.key }
            .map { KindRow(id: $0.key, kind: $0.key, count: $0.value) }
    }
}

struct PackDetailView: View {
    let packID: UUID
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var library: Library
    @State private var confirmingDelete = false
    @State private var worldName = ""

    private var pack: InstalledPack? {
        library.packs.first { $0.id == packID }
    }

    var body: some View {
        Group {
            if let pack {
                content(pack)
            } else {
                ContentUnavailableView("资源包已删除", systemImage: "trash")
            }
        }
        .navigationTitle(pack?.displayName ?? "资源包")
        .navigationBarTitleDisplayMode(.inline)
    }

    @ViewBuilder
    private func content(_ pack: InstalledPack) -> some View {
        Form {
            Section {
                Button {
                    model.play(pack)
                } label: {
                    Label("开始游戏", systemImage: "play.fill")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .disabled(!pack.installed)
            }

            Section(header: Text("资源包")) {
                LabeledContent("Minecraft 版本", value: pack.minecraftVersion)
                LabeledContent("引擎 ABI", value: "\(pack.engineAPI)")
                LabeledContent("条目", value: "\(pack.entryCount)")
                LabeledContent("体积", value: ByteCountFormatter.string(fromByteCount: Int64(pack.packBytes), countStyle: .file))
                LabeledContent("覆盖度", value: "\(String(format: "%.1f", pack.coverage * 100))%")
                LabeledContent("manifest", value: String(pack.manifestSHA256.prefix(16)) + "…")
            }

            Section(header: Text("条目构成")) {
                ForEach(KindRow.rows(from: pack.kinds)) { row in
                    LabeledContent(row.kind, value: "\(row.count)")
                }
            }

            if let jvm = pack.jvmSummary {
                Section(header: Text("仍是 Java 的部分")) {
                    LabeledContent("类", value: "\(jvm.classes)")
                    LabeledContent("方法", value: "\(jvm.methods)")
                    LabeledContent("字节码", value: ByteCountFormatter.string(fromByteCount: Int64(jvm.bytecodeBytes), countStyle: .file))
                    LabeledContent("调用边", value: "\(jvm.callEdges)")
                    if !jvm.topTargets.isEmpty {
                        Text("被调用最多的目标（Rust 侧需要对应实现）：")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        ForEach(jvm.topTargets) { target in
                            HStack {
                                Text("\(target.owner).\(target.method)")
                                    .font(.caption.monospaced())
                                    .lineLimit(1)
                                Spacer()
                                Text("\(target.callSites)")
                                    .font(.caption.monospacedDigit())
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }

            if let session = model.session, session.handle != nil {
                let packs = session.handle?.shaderPacks ?? []
                if !packs.isEmpty {
                    Section(header: Text("着色器包")) {
                        ForEach(packs) { pack in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(pack.id)
                                Text(pack.summary)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                if !pack.supported {
                                    Text("接口超出引擎能力：不会被执行，仅列出")
                                        .font(.caption2)
                                        .foregroundStyle(.orange)
                                }
                            }
                        }
                    } footer: {
                        Text("渲染器尚未执行自定义着色器；这里列出包声明的接口与缺口（docs/SHADERS.md）。")
                    }
                }
            }

            Section(header: Text("世界")) {
                let worlds = library.worlds(for: pack.id)
                if worlds.isEmpty {
                    Text("还没有存档").foregroundStyle(.secondary)
                } else {
                    ForEach(worlds) { world in
                        Button {
                            model.loadWorld(world)
                        } label: {
                            HStack {
                                Label(world.name, systemImage: "map")
                                Spacer()
                                Text(world.savedAt, style: .date)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                    .onDelete { indexSet in
                        for index in indexSet { library.removeWorld(worlds[index].id) }
                    }
                }
                HStack {
                    TextField("新存档名称", text: $worldName)
                    Button("保存当前世界") {
                        let name = worldName.isEmpty ? "世界 \(worlds.count + 1)" : worldName
                        model.saveWorld(for: pack, name: name)
                        worldName = ""
                    }
                    .disabled(model.session == nil)
                }
            }

            Section {
                Button("删除资源包", role: .destructive) { confirmingDelete = true }
            } footer: {
                Text("删除会一并移除该资源包的所有存档。")
            }
        }
        .confirmationDialog("删除「\(pack.displayName)」？", isPresented: $confirmingDelete) {
            Button("删除", role: .destructive) { library.remove(pack.id) }
            Button("取消", role: .cancel) {}
        }
    }
}