//
//  Settings.swift
//  HorizonPad
//
//  Small, explicit settings surface. There is no telephone-home switch here
//  because there is no telephone: the app ships no networking code at all.
//
//  Backed by UserDefaults directly (not @AppStorage, which is a View property
//  wrapper and does not belong on a class).
//

import Combine
import Foundation

@MainActor
final class Settings: ObservableObject {
    static let shared = Settings()

    enum Language: String, CaseIterable, Identifiable {
        case system, en, zh
        var id: String { rawValue }
        var label: String {
            switch self {
            case .system: "跟随系统"
            case .en: "English"
            case .zh: "简体中文"
            }
        }
    }

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        defaults.register(defaults: [
            Keys.renderScale: 1.0,
            Keys.drawRadius: 1,
            Keys.showStats: true,
            Keys.haptics: true,
            Keys.language: Language.system.rawValue,
            Keys.noAI: false,
            Keys.keepScreenAwake: true,
        ])
    }

    private enum Keys {
        static let renderScale = "renderScale"
        static let drawRadius = "drawRadius"
        static let showStats = "showStats"
        static let haptics = "haptics"
        static let language = "language"
        static let noAI = "noAI"
        static let keepScreenAwake = "keepScreenAwake"
    }

    var renderScale: Double {
        get { defaults.double(forKey: Keys.renderScale) }
        set {
            objectWillChange.send()
            defaults.set(newValue.clamped(to: 0.5...1.0), forKey: Keys.renderScale)
        }
    }

    var drawRadius: Int {
        get { defaults.integer(forKey: Keys.drawRadius) }
        set {
            objectWillChange.send()
            defaults.set(min(max(newValue, 1), 4), forKey: Keys.drawRadius)
        }
    }

    var showStats: Bool {
        get { defaults.bool(forKey: Keys.showStats) }
        set {
            objectWillChange.send()
            defaults.set(newValue, forKey: Keys.showStats)
        }
    }

    var haptics: Bool {
        get { defaults.bool(forKey: Keys.haptics) }
        set {
            objectWillChange.send()
            defaults.set(newValue, forKey: Keys.haptics)
        }
    }

    var keepScreenAwake: Bool {
        get { defaults.bool(forKey: Keys.keepScreenAwake) }
        set {
            objectWillChange.send()
            defaults.set(newValue, forKey: Keys.keepScreenAwake)
        }
    }

    var noAI: Bool {
        get { defaults.bool(forKey: Keys.noAI) }
        set {
            objectWillChange.send()
            defaults.set(newValue, forKey: Keys.noAI)
        }
    }

    var language: Language {
        get { Language(rawValue: defaults.string(forKey: Keys.language) ?? "") ?? .system }
        set {
            objectWillChange.send()
            defaults.set(newValue.rawValue, forKey: Keys.language)
        }
    }

    var locale: Locale {
        switch language {
        case .system: .autoupdatingCurrent
        case .en: Locale(identifier: "en")
        case .zh: Locale(identifier: "zh-Hans")
        }
    }
}

extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self {
        min(max(self, range.lowerBound), range.upperBound)
    }
}