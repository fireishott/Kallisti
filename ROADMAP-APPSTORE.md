# Herald - Roadmap to App Store Release

**Current version:** 1.2.1 (build 8)
**Target:** Public App Store release with Lite (free) + Pro (one-time purchase)
**License:** MIT (always open source, always buildable from Xcode)
**Architecture:** xcodegen (project.yml is source of truth, regenerate with `xcodegen generate`)

---

## Phase 1: Build Configuration Fixes ✅ DONE

- [x] Deployment target: iOS 18.0
- [x] Bundle ID: `net.fihonline.herald`
- [x] App Group: `group.net.fihonline.herald`
- [x] Development Team: 58U7UPFS53
- [x] iOS 26 API availability guards (SpeechService, glassEffect, scrollEdgeEffect)
- [x] Widget Info.plist version alignment

---

## Phase 2: StoreKit 2 Integration (One-Time Purchase)

### 2.1 Product Definition
**Create:** `Herald/IAP/ProductStore.swift`
- Product ID: `"net.fihonline.herald.pro"`
- StoreKit 2 `Product.products(for:)` lookup

### 2.2 Purchase Flow
**Create:** `Herald/IAP/PurchaseManager.swift`
- `@Observable` class wrapping StoreKit 2
- `loadProducts()`, `purchasePro()`, `restorePurchases()`
- `isProUnlocked` computed property

### 2.3 Feature Gating (Lite vs Pro)

**Lite (free):**
- Chat (text only, limited to 50 messages/day)
- Basic model selection (1-2 models)
- Session management
- Settings
- Onboarding + pairing

**Pro (one-time, $14.99):**
- Unlimited chat messages
- All model support
- Voice mode (WebRTC)
- Mimo TTS
- HealthKit integration
- Location services
- CoreMotion
- CarPlay
- Widgets (Live Activity, Health, Status)
- Skills browser
- Cron jobs
- Inbox
- Canvas
- Capture

### 2.4 Upgrade UI
**Create:** `Herald/Features/Upgrade/UpgradeSheetView.swift`

---

## Phase 3: App Store Assets

### 3.1 Screenshots (Required)
- 6.7" iPhone (iPhone 15 Pro Max): 3 screenshots
- 6.1" iPhone: 3 screenshots
- 12.9" iPad: 3 screenshots

### 3.2 Privacy Policy
**Host at:** `https://gocfwd.net/herald/privacy`

### 3.3 App Description
- Category: Productivity
- Subtitle: "Your self-hosted AI companion"

### 3.4 Keywords
```
ai,assistant,self-hosted,privacy,chat,voice,health,carplay,hermes,local
```

---

## Phase 4: TestFlight Beta

### 4.1 Beta Testing Checklist
- [ ] Onboarding flow end-to-end
- [ ] Pairing with relay
- [ ] Chat streaming
- [ ] Voice mode
- [ ] TTS
- [ ] Health data sync
- [ ] Location updates
- [ ] Widgets
- [ ] CarPlay
- [ ] Pro purchase flow (sandbox)
- [ ] Restore purchases
- [ ] No crashes
- [ ] Background modes
- [ ] iPad layout
- [ ] iPhone drawer

---

## Phase 5: App Store Submission

### 5.1 Review Notes for Apple
Explain self-hosted architecture clearly. All data stays on user's infrastructure.

### 5.2 Export Compliance
Standard HTTPS encryption, no custom crypto.
