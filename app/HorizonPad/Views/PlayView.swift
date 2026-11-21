//
//  PlayView.swift
//  HorizonPad
//

import SwiftUI

struct PlayView: View {
    let packID: UUID
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var library: Library
    @State private var showStats = true

    private var pack: InstalledPack? {
        library.packs.first { $0.id == packID }
    }

    var body: some View {
        Group {
            if let session = model.session, session.renderer != nil {
                ZStack(alignment: .topLeading) {
                    PlaySurface(session: session)
                    if showStats {
                        StatsOverlay(stats: session.stats, pack: pack)
                            .padding(16)
                            .allowsHitTesting(false)
                    }
                }
            } else {
                ContentUnavailableView {
                    Label("会话未启动", systemImage: "exclamationmark.triangle")
                } description: {
                    Text("返回上一页重新点击「开始游戏」。")
                }
            }
        }
        .navigationTitle(pack?.displayName ?? "游戏")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    showStats.toggle()
                } label: {
                    Image(systemName: showStats ? "gauge.with.dots.needle.67percent" : "gauge")
                }
                Button {
                    model.stopSession()
                } label: {
                    Image(systemName: "stop.fill")
                }
            }
        }
        .onDisappear {
            model.stopSession()
        }
    }
}

private struct StatsOverlay: View {
    let stats: GameHandle.Stats
    let pack: InstalledPack?

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            row("tick", "\(stats.ticks)")
            row("面", "\(stats.quads)")
            row("方块", "\(stats.nonAir)")
            row("包", pack?.minecraftVersion ?? "?")
        }
        .font(.caption2.monospacedDigit())
        .padding(10)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 10))
    }

    private func row(_ key: String, _ value: String) -> some View {
        HStack(spacing: 8) {
            Text(key).foregroundStyle(.secondary)
            Text(value)
        }
    }
}