//
//  Log.swift
//  HorizonPad
//
//  Thin wrapper over OSLog plus an in-memory ring buffer the Diagnostics screen
//  shows and exports. Nothing here leaves the device unless the user taps
//  "导出".
//

import Combine
import Foundation
import os

enum LogLevel: String, Sendable {
    case debug, info, warning, error
}

struct LogLine: Identifiable, Sendable {
    let id = UUID()
    let date: Date
    let level: LogLevel
    let text: String
}

@MainActor
final class LogStore: ObservableObject {
    static let shared = LogStore()
    private let logger = Logger(subsystem: "dev.playhorizon.pad", category: "app")
    private let capacity = 600

    @Published private(set) var lines: [LogLine] = []

    func write(_ level: LogLevel, _ text: String) {
        logger.log(level: level == .error ? .error : .info, "\(text, privacy: .public)")
        lines.append(LogLine(date: Date(), level: level, text: text))
        if lines.count > capacity {
            lines.removeFirst(lines.count - capacity)
        }
    }

    func debug(_ t: String) { write(.debug, t) }
    func info(_ t: String) { write(.info, t) }
    func warn(_ t: String) { write(.warning, t) }
    func error(_ t: String) { write(.error, t) }

    func exportText(environment: [String: String]) -> String {
        var out = "# HorizonPad 诊断日志\n"
        out += "# 生成于 \(ISO8601DateFormatter().string(from: Date()))\n"
        for (k, v) in environment.sorted(by: { $0.key < $1.key }) {
            out += "# \(k): \(v)\n"
        }
        out += "\n"
        let fmt = DateFormatter()
        fmt.dateFormat = "HH:mm:ss.SSS"
        for line in lines {
            out += "[\(fmt.string(from: line.date))] \(line.level.rawValue.uppercased()) \(line.text)\n"
        }
        return out
    }

    func clear() { lines.removeAll() }
}