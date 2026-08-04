# Kallisti v2.5.1 — Implementation Map

## Replace / add repository assets

Suggested destination in repo:

```text
docs/assets/rebrand/
  icons/
  website/
  logos/
  wallpaper/
```

For the app asset catalog:
- Use `assets/icon-1024.png` as the full-bleed App Store source
- Use the goddess medallion as `AppIconImage` for low-opacity wallpaper treatment
- Use `assets/wallpaper-default.png` as `KallistiBrandField` background in chat view

---

## README recommendations

1. Replace current top brand mark with a Kallisti banner (obsidian bg, platinum wordmark, goddess seal)
2. Add device showcase after "What is Kallisti?"
3. Feature strip above feature table
4. Set repository social preview to OG image from assets

---

## Website recommendations

- Hero: dark obsidian bg, goddess illustration watermark, platinum KALLISTI wordmark
- Open Graph: `assets/og-dark.png` (1200x630)
- Feature section: platinum-on-obsidian feature cards
- Device showcase: app screenshots in device frames
- Footer: obsidian banner

---

## App Store screenshot order (suggested)

1. Rich Chat — Kallisti Dark theme
2. Talk Naturally — voice mode
3. Stay Ahead — inbox / action center
4. Context, Kept Private — self-hosted angle
5. Think in Ink — notes
6. To the Most Beautiful — brand/hero screen

---

## v2.5.1 rollout additions

- Treat this package as the source of truth for the Kallisti v2.5.1 visual identity
- Wire `BUILDER_THEME_PROMPT.md` into your Claude Code implementation workflow
- Add user-visible **Kallisti OLED** and **Kallisti Light** options in Appearance settings
- Update release notes / changelog to mention the rebrand from Herald to Kallisti
- Prefer iOS screenshot set for App Store / landing page / repo previews

---

## File inventory — this package

```text
kallisti-rebrand-v2.5.1/
  assets/
    icon-1024.png          ← approved app icon (Greek goddess illustration)
    icon-{size}.png        ← all required iOS/web sizes
    favicon-16.png
    favicon-32.png
    wallpaper-default.png  ← 10% opacity goddess on black (chat wallpaper)
    wallpaper-default-full.png  ← full opacity reference
    brand-mark.png
    brand-mark-dark.png
    brand-mark-light.png
    color-palette.png
    contact-sheet.png
    og-image.png
    og-dark.png
    og-light.png
    social-avatar.png
    social-avatar-light.png
    splash-screen.png
    splash-dark.png
    splash-light.png
  docs/
    BRAND_SYSTEM.md           ← full brand identity spec
    BUILDER_THEME_PROMPT.md   ← Claude Code implementation prompt
    IMPLEMENTATION_MAP.md     ← this file
    THEMES.md                 ← token reference table
    rebrand-instructions.md   ← Herald→Kallisti step-by-step
    rebrand-gaps.md           ← current Herald remnants to fix
    CHANGELOG.md
    README.md
  website/
    index.html
    style.css
    assets/ (icon sizes for web)
```
