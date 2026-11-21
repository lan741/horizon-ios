//
//  HorizonPadApp.swift
//  HorizonPad
//

import SwiftUI

@main
struct HorizonPadApp: App {
    @StateObject private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .environmentObject(Library.shared)
                .environmentObject(Settings.shared)
                .environment(\.locale, Settings.shared.locale)
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background:
                model.session?.stop()
            case .active:
                model.session?.start()
            default:
                break
            }
        }
    }
}