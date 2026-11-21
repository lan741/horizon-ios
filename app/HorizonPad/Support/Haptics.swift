//
//  Haptics.swift
//  HorizonPad
//
//  One place that touches UIKit's feedback generators, so the engine and views
//  can ask for a tap without importing UIKit themselves.
//

import UIKit

enum Haptics {
    /// Light tick: block break/place, UI selection.
    @MainActor
    static func tap() {
        guard Settings.shared.haptics else { return }
        guard UIDevice.current.userInterfaceIdiom == .pad || UIDevice.current.userInterfaceIdiom == .phone else { return }
        let generator = UIImpactFeedbackGenerator(style: .light)
        generator.prepare()
        generator.impactOccurred(intensity: 0.6)
    }

    /// Heavier confirmation: world saved, import finished.
    @MainActor
    static func success() {
        UINotificationFeedbackGenerator().notificationOccurred(.success)
    }
}