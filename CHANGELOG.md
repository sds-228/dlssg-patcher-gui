# Changelog

## 2.0.2

### Fixed
- Fixed factory-equivalent per-game settings being treated as custom, which could show a `[C]` status and reapply custom settings on patch/repatch until Config was reset to factory.

## 2.0.1

### Changed
- Updated mod support to **dlssg_for_sm86 0.4.2** (pinned release asset and SHA-256; same bundle layout as 0.4.1).
- Config remains available for cached bundles **0.4.1+** (including 0.4.2) with the same per-game settings.

### Fixed
- Corrected XeSS `UseHudless=Auto` guidance for 0.4.2: `Auto` now behaves the same as `0` (interpolates the whole image, including UI), while `1` still requires a HUD-less image.

## 0.2.0

### Added
- **Per-game Config (⚙):** Engine (DLSS/XeSS), frame multiplier, DLSS preset/optimized, XeSS options, and Super Resolution mode per game (applied on patch/repatch; defaults unchanged).
- **Per-game Super Resolution (opt-in):** on-demand SR payload (~180 MB), pinned to the cached mod bundle.
- **Status indicators:** compact `[C]`, `[SR]`, `[C+SR]` markers with full details in tooltip/log.
- **Driver guidance:** WDDM/driver detection with R535+ (30-series) and R580+ (20-series) guidance.

### Changed
- Updated mod support to **dlssg_for_sm86 0.4.1**; new settings require cached bundle 0.4.1+.
- Improved settings handling/migration and INI composition; Reset to factory now clears saved settings and restores true factory defaults (including removing SR).
