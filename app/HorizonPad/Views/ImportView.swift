//
//  ImportView.swift
//  HorizonPad
//

import SwiftUI
import UniformTypeIdentifiers

struct ImportView: View {
    @EnvironmentObject private var model: AppModel
    @StateObject private var controller = ImportController()
    @State private var showPicker = false

    var body: some View {
        Form {
            Section(header: Text("选择文件")) {
                Button {
                    showPicker = true
                } label: {
                    HStack {
                        Label("选择 Minecraft jar", systemImage: "doc.badge.plus")
                        Spacer()
                        if let url = controller.pendingURL {
                            Text(url.lastPathComponent).foregroundStyle(.secondary)
                        }
                    }
                }
                if controller.pendingURL != nil {
                    TextField("显示名称", text: $controller.displayName)
                    TextField("Minecraft 版本（如 26.3）", text: $controller.minecraftVersion)
                        .keyboardType(.numbersAndPunctuation)
                }
            }

            if case .porting = controller.phase {
                Section(header: Text("进度")) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(controller.progress.label).font(.callout)
                        ProgressView(value: controller.progress.fraction)
                        Text("\(controller.progress.done) / \(controller.progress.total)")
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                    }
                }
            }

            if case .failed(let message) = controller.phase {
                Section {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                }
            }

            if case .done = controller.phase {
                Section {
                    Label("导入完成", systemImage: "checkmark.seal.fill")
                        .foregroundStyle(.green)
                        .onAppear { Haptics.success() }
                    Button("再导入一个") { controller.reset() }
                }
            }

            Section {
                Button {
                    Task { await controller.run() }
                } label: {
                    Text("开始编译").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .disabled(controller.pendingURL == nil || controller.phase == .porting || controller.phase == .copying)
            } footer: {
                Text("文件只在本机处理。编译在本机完成，不上传任何内容；应用没有网络权限。")
            }
        }
        .navigationTitle("导入")
        .fileImporter(
            isPresented: $showPicker,
            allowedContentTypes: ImportController.allowedTypes,
            allowsMultipleSelection: false
        ) { result in
            switch result {
            case .success(let urls):
                if let url = urls.first { controller.begin(url: url) }
            case .failure(let error):
                model.lastError = error.localizedDescription
            }
        }
        .onAppear { ImportController.current = controller }
        .onDisappear { ImportController.current = nil }
    }
}