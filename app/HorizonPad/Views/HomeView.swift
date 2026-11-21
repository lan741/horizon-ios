//
//  HomeView.swift
//  HorizonPad
//

import SwiftUI

struct HomeView: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var library: Library
    @State private var showImporter = false

    private let columns = [GridItem(.adaptive(minimum: 280), spacing: 18)]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                header

                if library.packs.isEmpty {
                    emptyState
                } else {
                    LazyVGrid(columns: columns, spacing: 18) {
                        ForEach(library.packs) { pack in
                            PackCard(pack: pack)
                                .onTapGesture { model.path.append(.packDetail(pack.id)) }
                        }
                    }
                }

                howItWorks
            }
            .padding(24)
        }
        .navigationTitle("HorizonPad")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    model.path.append(.download)
                } label: {
                    Label("下载版本", systemImage: "arrow.down.circle")
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    model.path.append(.importJar)
                } label: {
                    Label("导入", systemImage: "plus")
                }
            }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("把《我的世界》Java 版搬到 iPad 上跑")
                .font(.largeTitle.bold())
            Text("你提供自己的 client.jar，应用在设备上把方块状态、模型几何、贴图、语言表和数据包编译成原生资源包，再交给内置的 Rust 引擎渲染。Java 代码不会执行，也不会被翻译后悄悄跑起来；剩余多少 Java 类，诊断页会如实列出。")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 14) {
            Image(systemName: "shippingbox.and.arrow.backward")
                .font(.system(size: 42))
                .foregroundStyle(.tint)
            Text("还没有资源包")
                .font(.title3.bold())
            Text("导入电脑上的 `.minecraft/versions/<版本>/<版本>.jar`，或者直接从镜像下载一个版本——两者都在设备上完成编译。")
                .foregroundStyle(.secondary)
            HStack(spacing: 12) {
                Button {
                    model.path.append(.importJar)
                } label: {
                    Label("导入 Minecraft jar", systemImage: "square.and.arrow.down")
                }
                .buttonStyle(.borderedProminent)
                Button {
                    model.path.append(.download)
                } label: {
                    Label("下载游戏版本", systemImage: "arrow.down.circle")
                }
                .buttonStyle(.bordered)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(28)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 18))
    }

    private var howItWorks: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("它是怎么工作的").font(.title3.bold())
            ForEach(Self.pipeline) { step in
                HStack(alignment: .top, spacing: 12) {
                    Image(systemName: step.icon)
                        .frame(width: 26)
                        .foregroundStyle(.tint)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(step.title).font(.callout.weight(.semibold))
                        Text(step.detail).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 16))
    }

    struct Step: Identifiable {
        let id: Int
        let title: String
        let detail: String
        let icon: String
    }

    private static let pipeline: [Step] = [
        Step(id: 0, title: "读取 jar",
             detail: "自研 inflate + ZIP 解析器，逐项校验 CRC，不解压到磁盘。",
             icon: "doc.zipper"),
        Step(id: 1, title: "编译方块状态与模型",
             detail: "variants / multipart 解析，parent 链合并，元素旋转烘焙成四边形。",
             icon: "cube.transparent"),
        Step(id: 2, title: "打包贴图与语言",
             detail: "PNG 原样保留并校验魔数；语言表转成二进制字符串表，启动不再解析 JSON。",
             icon: "textformat"),
        Step(id: 3, title: "写出 .hzp",
             detail: "单文件、无压缩、按名索引，引擎读取时不需要再解压。",
             icon: "archivebox"),
        Step(id: 4, title: "Rust 引擎运行",
             detail: "调色板压缩区块、AABB 碰撞、面剔除，Swift 只负责 Metal 绘制与触摸。",
             icon: "cpu"),
    ]
}

private struct PackCard: View {
    let pack: InstalledPack

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(pack.displayName)
                    .font(.headline)
                    .lineLimit(1)
                Spacer()
                if !pack.installed {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                        .help("资源包文件缺失")
                }
            }
            HStack(spacing: 6) {
                Tag(text: "MC \(pack.minecraftVersion)")
                Tag(text: "\(pack.entryCount) 项")
                Tag(text: "\(String(format: "%.0f", pack.coverage * 100))% 覆盖")
            }
            ProgressView(value: pack.coverage)
                .tint(pack.coverage > 0.95 ? .green : .orange)
            Text(summaryLine)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(2)
            HStack {
                Text(ByteCountFormatter.string(fromByteCount: Int64(pack.packBytes), countStyle: .file))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                Spacer()
                if let played = pack.lastPlayed {
                    Text(played, style: .relative)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 16))
    }

    private var summaryLine: String {
        let parts = pack.kinds
            .filter { $0.value > 0 && $0.key != "manifest" }
            .sorted { $0.key < $1.key }
            .map { "\($0.key) \($0.value)" }
        return parts.isEmpty ? "暂无统计" : parts.joined(separator: " · ")
    }
}

struct Tag: View {
    let text: String
    var body: some View {
        Text(text)
            .font(.caption2.weight(.medium))
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(.quaternary, in: Capsule())
    }
}