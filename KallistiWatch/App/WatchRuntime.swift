//
//  WatchRuntime.swift
//  KallistiWatch
//
//  Builds the transports the conversation screen uses, from the pairing the
//  phone handed over. One place, so the screen and the intent cannot disagree
//  about which origin or which token is in play.
//

import Foundation

/// What the screen needs to talk to a host.
struct WatchRuntime {
    let chooser: WatchTransportChooser
    /// The transport that can stream frames, when there is one.
    let streamer: (any WatchTransport)?

    /// Build from the stored pairing. With no pairing the relay is still there -
    /// it is the path that pairs in the first place.
    @MainActor
    static func make(store: WatchPairingStore = WatchPairingStore()) -> WatchRuntime {
        seedFromLaunchArgument(store: store)
        let relay = WatchPhoneRelay()
        guard let credentials = try? store.credentials(),
              let origin = credentials.origin else {
            return WatchRuntime(
                chooser: WatchTransportChooser(relay: relay, direct: nil),
                streamer: nil
            )
        }
        let direct = WatchDirectHTTP(
            origin: origin,
            token: credentials.accessToken,
            refresh: {
                // One refresh, then the store holds the new pair. A failure here
                // returns nil and the request answers `reauth`.
                guard let fresh = try? await WatchDirectHTTP.refreshPairing(
                    origin: origin,
                    refreshToken: credentials.refreshToken
                ) else {
                    return nil
                }
                try? store.save(
                    WatchCredentials(
                        accessToken: fresh.accessToken,
                        refreshToken: fresh.refreshToken,
                        deviceID: credentials.deviceID,
                        origin: origin
                    )
                )
                return fresh.accessToken
            }
        )
        return WatchRuntime(
            chooser: WatchTransportChooser(relay: relay, direct: direct),
            streamer: direct
        )
    }

    /// Hand a pairing over at launch, for a wrist whose phone screen has not run
    /// yet: `simctl launch <udid> <bundle> -WatchPairingBootstrap '<json>'`. It
    /// only ever fills an empty store, so a real pairing always wins.
    @MainActor
    private static func seedFromLaunchArgument(store: WatchPairingStore) {
        guard (try? store.credentials()) == nil,
              let raw = UserDefaults.standard.string(forKey: "WatchPairingBootstrap"),
              let data = raw.data(using: .utf8),
              let credentials = try? JSONDecoder().decode(WatchCredentials.self, from: data)
        else {
            return
        }
        try? store.save(credentials)
    }
}
