//
//  WatchStateSnapshot.swift
//  KallistiWatch
//
//  The one value the app and its complication share: the last line of the
//  answer, the word the orb was showing, and when it was written. The app writes
//  it; the complication reads it. Both targets compile this file.
//

import Foundation

struct WatchStateSnapshot: Codable, Sendable, Equatable {
    var lastLine: String
    var stateWord: String
    var updatedAt: Date

    /// One key, in the app group both targets already share.
    static let defaultsKey = "watch.state.snapshot"
    static let appGroup = "group.net.fihonline.kallisti"

    /// The longest line a face has room for.
    static let lineLimit = 120

    static func read() -> WatchStateSnapshot {
        guard
            let data = UserDefaults(suiteName: appGroup)?.data(forKey: defaultsKey),
            let snapshot = try? JSONDecoder().decode(WatchStateSnapshot.self, from: data)
        else {
            return WatchStateSnapshot(lastLine: "", stateWord: "idle", updatedAt: .distantPast)
        }
        return snapshot
    }

    static func write(lastLine: String, stateWord: String) {
        let snapshot = WatchStateSnapshot(
            lastLine: String(lastLine.prefix(lineLimit)),
            stateWord: stateWord,
            updatedAt: Date()
        )
        guard let data = try? JSONEncoder().encode(snapshot) else { return }
        UserDefaults(suiteName: appGroup)?.set(data, forKey: defaultsKey)
    }
}
