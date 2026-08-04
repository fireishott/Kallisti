# Kallisti Theme System - Code Reference

Based on Herald 2.4.4 ThemePalette architecture.

---

## ThemePalette Token Map

All themes use the same ThemePalette struct. Default values from the init handle
anything not explicitly set.

| Token | Kallisti (Dark) | Kallisti OLED | Kallisti Light |
|-------|-----------------|---------------|----------------|
| background | #0C0C10 | #050507 | #FAFAFA |
| foreground | #F0F2F5 | #F0F2F5 | #0C0C10 |
| secondaryForeground | #A8ADB5 | #A8ADB5 | #4A4D55 |
| surface | #1C1D22 | #0A0A0E | #FFFFFF |
| divider | #2A2B33 | #1C1D22 | #E0E0E0 |
| deepInk | #000000 | #000000 | #FFFFFF |
| panel | #16181C | #0A0A0E | #FFFFFF |
| surfaceRaised | #22232A | #0F0F14 | #F5F5F5 |
| surfaceSelected | #2A2B33 | #0F0F14 | rgba(accentHot, 0.16) |
| tertiaryForeground | #6B7078 | #6B7078 | #8A909A |
| accent | #C8CCD2 | #C8CCD2 | #0C0C10 |
| accentHot | #E5E8EC | #E5E8EC | #1C1D22 |
| success | #41C98E | #41C98E | #41C98E |
| warning | #D9AF53 | #D9AF53 | #D9AF53 |
| danger | #CF4D57 | #CF4D57 | #CF4D57 |
| textureOpacity | 0.04 | 0.015 | 0.06 |
| watermarkOpacity | 0.12 | 0.06 | 0.10 |
| prefersSharpEdges | false | true | false (OLED: true) |

---

## HeraldAppearance (Settings Picker)

| Appearance | Preset | Color Scheme |
|------------|--------|--------------|
| System | kallisti | system |
| Kallisti | kallisti | dark |
| Kallisti OLED | kallistiOLED | dark |
| Kallisti Light | kallisti | light |

---

## Legacy Presets (Retained)

Keep midnight, ember, mono, cyberpunk, slate as secondary options.
Update their accent colors if desired, or leave as-is.

---

## Files to Modify

1. `Herald/Core/HeraldTheme.swift` → Rename to `KallistiTheme.swift`
2. `Herald/Core/Theme.swift` → Update ThemePreset, HeraldAppearance, ThemePalette refs
3. `Herald/Core/ThemeManager.swift` → Default preset change
4. All view files referencing `HeraldTheme.*` → `KallistiTheme.*`
5. All view files referencing `Design.Colors` → No change (resolves through theme)
