# MONETIZATION.md
### Oh yeah, by the way...

**DO NOT IMPLEMENT DURING BETA.** Beta testers get the full experience.
This ships after beta feedback is collected and resolved.

---

## The Deal

No subscriptions. Ever. One-time purchase.

Open source always. MIT license. Clone it, compile it, run it. Pro features included.
If you build it from source, you get Pro for free. That's the open source promise.
The App Store purchase is a convenience, not a requirement.

---

## Free Tier

No account. No credit card. No trial timer. No nag screen.

- Pair with your Hermes instance
- Text chat, unlimited
- Session management
- Model selection
- Settings, themes, wallpaper

Free is free forever. This is the real product. People should be able to use Herald free and be happy.

## Pro Tier - $24.99 One-Time

- Voice mode (WebRTC)
- Mimo TTS (8 voices, auto-speak)
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
- WatchOS (coming soon)

---

## Why This Works

Free tier builds habit and daily dependency.
Pro features solve problems users have already felt ("I wish I could use voice while driving").
Purchase feels like unlocking something they want, not paying to keep something they have.
No pressure, no timers, no anxiety. Buy when ready.

---

## Launch Pricing

- **Launch:** $14.99 (2-3 weeks, collect early reviews and social proof)
- **Final:** $24.99 (permanent from launch price end)

Announce price change 48-72 hours in advance on GitHub, Reddit, Twitter.
"Launch price ending" announcement is itself a marketing beat - second wave of sales.
Description note: "Launch price - regular price $24.99"

---

## Open Source Pro

Users who compile from source get Pro features automatically.
No license key, no activation, no gate. The source IS the unlock.
Non-negotiable. MIT license, full features, build it yourself.

---

## Implementation (Post-Beta Only)

1. StoreKit 2 non-consumable product: `"net.fihonline.herald.pro"`
2. `ProFeature` enum gating all Pro features
3. `isProUnlocked` check in relevant views/services
4. `UpgradeSheetView` (clean, non-aggressive, shows what Pro includes)
5. SettingsScreen "Upgrade to Pro" section (hidden when already Pro)
6. Free tier limits: text chat only, no voice, no sensors, no widgets
