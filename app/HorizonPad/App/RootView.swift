//
//  RootView.swift
//  HorizonPad
//

import SwiftUI

struct RootView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        NavigationSplitView {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 240, ideal: 280, max: 340)
        } detail: {
            NavigationStack(path: $model.path) {
                HomeView()
                    .navigationDestination(for: AppModel.Route.self) { route in
                        switch route {
                        case .importJar: ImportView()
                        case .download: DownloadView()
                        case .diagnostics: DiagnosticsView()
                        case .settings: SettingsView()
                        case .packDetail(let id): PackDetailView(packID: id)
                        case .play(let id): PlayView(packID: id)
                        }
                    }
            }
        }
        .alert("出错了", isPresented: Binding(
            get: { model.lastError != nil },
            set: { if !$0 { model.lastError = nil } }
        )) {
            Button("好", role: .cancel) { model.lastError = nil }
        } message: {
            Text(model.lastError ?? "")
        }
    }
}

private struct Sidebar: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var library: Library

    var body: some View {
        List {
            Section(header: Text("Horizon")) {
                Label("资源包", systemImage: "shippingbox")
                    .badge(library.packs.count)
                Button {
                    model.path.append(.importJar)
                } label: {
                    Label("导入 Minecraft jar", systemImage: "square.and.arrow.down")
                }
            }
            Section(header: Text("最近")) {
                if library.packs.isEmpty {
                    Text("还没有导入任何资源包")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                ForEach(library.packs.prefix(6)) { pack in
                    Button {
                        model.path.append(.packDetail(pack.id))
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(pack.displayName).lineLimit(1)
                            Text("MC \(pack.minecraftVersion) · \(pack.entryCount) 项")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .buttonStyle(.plain)
                }
            }
            Section {
                Button { model.path.append(.diagnostics) } label: {
                    Label("诊断", systemImage: "stethoscope")
                }
                Button { model.path.append(.settings) } label: {
                    Label("设置", systemImage: "gearshape")
                }
            }
        }
        .listStyle(.sidebar)
    }
}