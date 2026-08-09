//
//  SharedGatewayStatus.swift
//  HeraldSupport
//
//  App Group-backed gateway status snapshot.  Mirrors the legacy
//  `GatewayState` so widget timelines and Control Center controls can
//  render a recent status without an HTTP round-trip — and so writes from
//  the main app's gateway refresh path land in the same suite that
//  Controls reads from.
//
//  Build 108 — Phase 3 §15A.
//

import Foundation
import os
import WidgetKit

/// Thread-safe gateway status snapshot.  Backed by the shared App Group
/// `UserDefaults` so all Herald binaries see the same snapshot the main
/// app last persisted.
public final class SharedGatewayStatus: @unchecked Sendable {
    /// Singleton instance.
    public static let shared = SharedGatewayStatus()

    private let defaults: UserDefaults?
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "SharedGatewayStatus")

    public init(suiteName: String = KallistiSupportConfiguration.appGroupIdentifier) {
        self.defaults = UserDefaults(suiteName: suiteName)
    }

    // MARK: - Read

    public var isConnected: Bool {
        defaults?.bool(forKey: prefix("connected")) ?? false
    }

    public var activeJobs: Int {
        defaults?.integer(forKey: prefix("activeJobs")) ?? 0
    }

    public var model: String? {
        defaults?.string(forKey: prefix("model"))
    }

    public var version: String? {
        defaults?.string(forKey: prefix("version"))
    }

    public var uptimeSeconds: Int {
        defaults?.integer(forKey: prefix("uptimeSeconds")) ?? 0
    }

    public var lastUpdated: Date? {
        defaults?.object(forKey: prefix("lastUpdated")) as? Date
    }

    // MARK: - Write

    /// Update one or more fields and (optionally) reload all widget
    /// timelines.  Pass `reloadWidgets: false` when calling from inside a
    /// widget process — `WidgetCenter.reloadAllTimelines()` is a no-op
    /// when the caller is itself a widget timeline.
    public func update(
        reloadWidgets: Bool = true,
        connected: Bool? = nil,
        activeJobs: Int? = nil,
        model: String? = nil,
        version: String? = nil,
        uptimeSeconds: Int? = nil
    ) {
        guard let defaults else {
            logger.error("App Group unavailable — gateway status update dropped")
            return
        }

        var changed = false
        if let v = connected { defaults.set(v, forKey: prefix("connected")); changed = true }
        if let v = activeJobs { defaults.set(v, forKey: prefix("activeJobs")); changed = true }
        if let v = model { defaults.set(v, forKey: prefix("model")); changed = true }
        if let v = version { defaults.set(v, forKey: prefix("version")); changed = true }
        if let v = uptimeSeconds { defaults.set(v, forKey: prefix("uptimeSeconds")); changed = true }

        if changed {
            defaults.set(Date(), forKey: prefix("lastUpdated"))
            if reloadWidgets {
                WidgetCenter.shared.reloadAllTimelines()
            }
        }
    }

    /// Drop the cached snapshot.  Called on disconnect / sign-out.
    public func clear(reloadWidgets: Bool = true) {
        guard let defaults else { return }
        for key in [
            "connected", "activeJobs", "model", "version", "uptimeSeconds", "lastUpdated",
        ] {
            defaults.removeObject(forKey: prefix(key))
        }
        if reloadWidgets {
            WidgetCenter.shared.reloadAllTimelines()
        }
    }

    private func prefix(_ name: String) -> String {
        KallistiSupportConfiguration.gatewayStatusDefaultsPrefix + name
    }
}
