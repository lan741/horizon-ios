//
//  SettingsView.swift
//  HorizonPad
//

import SwiftUI

struct SettingsView: View {
    @EnvironmentObject private var settings: Settings
    @EnvironmentObject private var library: Library

    var body: some View {
        Form {
            Section(header: Text("渲染")) {
                VStack(alignment: .leading) {
                    Text("渲染分辨率 \(String(format: "%.2f", settings.renderScale))×")
                    Slider(value: $settings.renderScale, in: 0.5...1.0, step: 0.05)
                }
                Stepper("绘制半径 \(settings.drawRadius) 区块", value: $settings.drawRadius, in: 1...4)
                Toggle("显示性能统计", isOn: $settings.showStats)
                Toggle("保持屏幕常亮", isOn: $settings.keepScreenAwake)
            }

            Section(header: Text("界面")) {
                Picker("语言", selection: $settings.language) {
                    ForEach(Settings.Language.allCases) { language in
                        Text(language.label).tag(language)
                    }
                }
                Toggle("触感反馈", isOn: $settings.haptics)
            }

            Section {
                Toggle("No AI（关闭全部 AI 功能）", isOn: $settings.noAI)
            } footer: {
                Text("本版本不含任何 AI 功能；该开关为兼容 Horizon 生态保留，开启后隐藏相关入口。")
            }

            Section(header: Text("存储")) {
                LabeledContent("资源包", value: "\(library.packs.count) 个")
                LabeledContent("占用", value: ByteCountFormatter.string(fromByteCount: Int64(library.totalBytes), countStyle: .file))
                LabeledContent("存档", value: "\(library.worlds.count) 个")
            }

            Section(header: Text("关于")) {
                LabeledContent("引擎 ABI", value: "\(Engine.apiVersion)")
                LabeledContent("版本", value: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?")
                Link("Horizon 项目", destination: URL(string: "https://playhorizon.dev")!)
            }

            Section {
                Text("本应用不包含 Mojang 的任何代码、贴图或声音。资源由用户自己的《我的世界》Java 版副本在设备上生成。Minecraft 是 Mojang 与 Microsoft 的商标，本应用与之无关联。")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .navigationTitle("设置")
    }
}