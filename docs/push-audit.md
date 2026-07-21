# Push Registration Flow Audit

## 1. How the app obtains the device token

The app registers for remote notifications via `UNUserNotificationCenter`. When APNs issues a device token, the system calls `application(_:didRegisterForRemoteNotificationsWithDeviceToken:)` in the AppDelegate. The raw `Data` token is hex-encoded to a `String` and passed to `AppContainer.registerPushTokenIfNeeded(_:)`.

The token is stored in `NotificationService.currentPushToken` and persisted in `UserDefaults` via the notification service.

## 2. How it registers with the relay

`AppContainer.registerPushTokenIfNeeded(_:)` (line 656) performs these checks:
1. Verifies the device is paired (`pairingStore.isPaired`)
2. Checks notifications are enabled (`settingsStore.settings.notificationsEnabled`)
3. Normalizes the token (trims whitespace)
4. Gets the current access token from `sessionStore`
5. Gets the `deviceID` from session state
6. Determines the push environment: `#if DEBUG` → `"development"`, else → `"production"`
7. Calls `PushRegistrationCoordinator.registerPushToken(...)` with the token, environment, and relay configuration

The coordinator has two paths:
- **Direct transport** (`shouldUseBroker == false`): POSTs to `/v1/push/register` with `{deviceId, transport: "direct", apnsToken, pushEnvironment, bundleId}`
- **Broker transport** (`shouldUseBroker == true`): First establishes a broker registration (App Attest challenge-response), then POSTs to `/v1/push/register` with `{deviceId, transport: "relay", pushEnvironment, bundleId, relayHandle, sendGrant, relayId, relayPublicKey, tokenDebugSuffix}`

## 3. How the relay stores registrations (schema)

Table `push_registrations` (SQLAlchemy model `PushRegistration`):

| Column | Type | Description |
|--------|------|-------------|
| `id` | UUID | Primary key |
| `device_id` | UUID FK | References `devices.id` |
| `transport` | text | `"direct"` or `"relay"` |
| `apns_token` | text | Raw APNs token (direct only) |
| `push_environment` | text | `"development"` or `"production"` |
| `bundle_id` | text | App bundle identifier |
| `relay_handle` | text | Opaque handle from broker (relay transport) |
| `send_grant` | text | Opaque grant from broker (relay transport) |
| `relay_id` | text | Relay identity ID (relay transport) |
| `relay_public_key` | text | Relay Ed25519 public key (relay transport) |
| `token_debug_suffix` | text | Last 8 chars of token for logging (relay transport) |
| `is_active` | bool | Whether registration is active |
| `last_registered_at` | datetime | Last registration timestamp |
| `created_at` | datetime | Creation timestamp |
| `updated_at` | datetime | Last update timestamp |

Registration is upserted per-device (`services.upsert_push_registration`). On update, all fields are overwritten and `is_active` is set to `True`.

## 4. How the relay sends push notifications

Two paths:

### Direct transport
`APNsClient.send_alert_push()` or `send_silent_push()` sends directly to Apple's APNs HTTP/2 API:
- Environment determines endpoint: `"development"` → `api.development.push.apple.com`, `"production"` → `api.push.apple.com`
- JWT bearer token auth (ES256 with .p8 key)
- Topic = `bundle_id`

### Broker transport
`push_broker_sender()` forwards the push request to the managed push broker, which holds the actual APNs credentials and sends on behalf of the relay.

### Trigger points
- **Message reply** (`maybe_send_message_push`): Iterates all active registrations for the user, skips foreground devices, sends alert push
- **Internal `/v1/push/send`**: Endpoint for connectors to send silent/alert pushes to all user devices
- **Push result handling**: `PushResult.TOKEN_INVALID` (410 Gone) → sets `is_active = False` on the registration

## 5. The short-circuit logic (FIXED)

**Before fix** (AppContainer.swift:685-688):
```swift
if notificationService.isPushTokenRegistered,
   notificationService.currentPushToken == normalizedToken {
    sessionStore.state.pushTokenRegistered = true
    return
}
```

This skipped re-registration when:
- The local `isPushTokenRegistered` flag was `true` (persists across launches)
- The local token matched the normalized token

**Problem:** If the relay deactivated the registration (e.g., APNs returned 410 Gone), the app would never re-register on subsequent launches because the local flag was still `true`.

**Fix:** Removed the short-circuit entirely. The app now always sends the registration request to the relay on launch. The relay's upsert is idempotent and reactivates deactivated registrations.

**Per-environment routing** (relay/app/main.py:1816):
```python
# Before: push_environment=settings.apns_environment  (relay's global setting)
# After:  push_environment=payload.pushEnvironment     (app-reported per-registration)
```
