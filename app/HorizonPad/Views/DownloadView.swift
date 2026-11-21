//
//  DownloadView.swift
//  HorizonPad
//
//  Download a game version from a mirror, inside the app.
//
//  The screen states the cost before the user commits: how many files, how many
//  megabytes, and that the result needs no further action to play. It also says
//  what the port does not carry, because a download that quietly drops entity
//  textures would look like a bug later.
//

import SwiftUI

struct DownloadView: View {
    @StateObject private var downloader = VersionDownloader()
    @EnvironmentObject private var library: Library
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Form {
            Section {
                Picker("下载源", selection: $downloader.mirrorID) {
                    ForEach(Mirror.all) { mirror in
                        Text(mirror.name).tag(mirror.id)
                    }
                }
                .pickerStyle(.segmented)
                Text(Mirror.named(downloader.mirrorID).note)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("镜像")
            }

            Section {
                Toggle("包含音效文件（体积大）", isOn: $downloader.selection.includeSounds)
                Toggle("包含着色器", isOn: $downloader.selection.includeShaders)
                Toggle("只显示正式版", isOn: $downloader.onlyReleases)
            } header: {
                Text("内容")
            } footer: {
                Text("默认只取方块与物品贴图、模型、方块状态、语言和音效目录；实体贴图、画、皮肤不在其中。")
            }

            Section {
                switch downloader.phase {
                case .loadingVersions:
                    ProgressView("读取版本清单…")
                case .downloading(let step, let done, let total):
                    VStack(alignment: .leading, spacing: 6) {
                        ProgressView(value: Double(done), total: Double(max(total, 1)))
                        Text("\(step)：\(done)/\(total)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                case .packing:
                    ProgressView("打包…")
                case .porting:
                    ProgressView("编译为引擎格式…")
                case .done(let id):
                    Label("完成", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    Text("已加入资源包列表：\(library.packs.first { $0.id == id }?.displayName ?? "")")
                        .font(.caption)
                    Button("返回") { dismiss() }
                case .failed(let message):
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                        .font(.callout)
                case .idle:
                    if downloader.versions.isEmpty {
                        Button("读取版本清单") {
                            Task { await downloader.loadVersions() }
                        }
                    } else {
                        Text("共 \(downloader.visibleVersions.count) 个版本，最新 \(downloader.manifestLatest)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            } header: {
                Text("进度")
            }

            if !downloader.visibleVersions.isEmpty {
                Section {
                    ForEach(downloader.visibleVersions.prefix(40)) { version in
                        Button {
                            Task { await downloader.download(version) }
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(version.id)
                                    Text(version.releaseTime.prefix(10))
                                        .font(.caption2)
                                        .foregroundStyle(.secondary)
                                }
                                Spacer()
                                if version.id == downloader.manifestLatest {
                                    Text("最新")
                                        .font(.caption2)
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 2)
                                        .background(Color.accentColor.opacity(0.15), in: Capsule())
                                }
                                Image(systemName: "arrow.down.circle")
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .disabled(downloader.phase.isBusy)
                    }
                } header: {
                    Text("版本")
                } footer: {
                    Text("下载在设备上完成：先去镜像取版本信息与资源索引，再按哈希逐个取文件并校验 SHA-1，最后本机编译成 .hzp。")
                }
            }
        }
        .navigationTitle("下载游戏版本")
        .task {
            if downloader.versions.isEmpty {
                await downloader.loadVersions()
            }
        }
    }
}