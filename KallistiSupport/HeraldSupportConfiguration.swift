//
//  HeraldSupportConfiguration.swift
//  HeraldSupport
//
//  Single source of truth for the cross-process identifiers used by the
//  Herald app, the widget extension, the control-center extension, and the
//  notification service extension. All values are extension-safe so the
//  shared module can be compiled with `APPLICATION_EXTENSION_API_ONLY=YES`.
//
//  Build 108 — Phase 3 §15A.
//

import Foundation

/// Stable cross-process identifiers.
///
/// These values are referenced from every consumer target (main app, widgets,
/// controls, intents, notification service). Changing them after a release
/// would orphan previously persisted App Group data and Keychain items, so
/// they are intentionally hard-coded constants rather than build settings.
public enum HeraldSupportConfiguration {
    /// App Group that all Herald binaries share. Both the URL (non-secret)
    /// and the cached gateway status (non-secret) live here.
    public static let appGroupIdentifier = "group.net.fihonline.herald"

    /// Shared Keychain access group for the relay access token.
    ///
    /// The Keychain access group MUST be provisioned in the Apple Developer
    /// Portal App IDs for the main app and the HeraldControls extension
    /// (and any other consumer that needs to read the token).  Until the
    /// portal is updated this is a placeholder identifier; the user-visible
    /// report documents the choice and the steps required to promote it.
    public static let keychainAccessGroup = "group.net.fihonline.herald.shared"

    /// Service identifier inside the shared Keychain access group.  Older
    /// builds used `net.fihonline.herald.session` (no access group); the
    /// migration routine in `SharedTokenMigrator` reads from that legacy
    /// service first before deleting it.
    public static let keychainServiceName = "net.fihonline.herald.session"

    /// Account name (inside the Keychain service) for the relay access
    /// token.  Reused from the legacy secure store so the migration is a
    /// copy/delete rather than a re-keying.
    public static let keychainAccessTokenAccount = "session.accessToken"

    /// App Group `UserDefaults` key for the relay base URL.  Non-secret by
    /// design — only the access token is held in the Keychain.
    public static let relayBaseURLDefaultsKey = "herald.relayBaseURL"

    /// Prefix for the App Group `UserDefaults` keys used by the cached
    /// gateway status.  See `SharedGatewayStatus` for the full key list.
    public static let gatewayStatusDefaultsPrefix = "gw."
}
