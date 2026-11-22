# Changelog

## 2.0.4

### Fixed
- Removed the overlapping Update section; patcher updates are now in the **Update** menu (near About) with check/download/restart actions.

## 2.0.3

### Added
- Patcher self-update: the main **Update** section checks for new `dlssg-patcher-gui` releases, notifies you when an update is available, and lets you download and apply it on restart (no automatic download or install). If in-place replacement is not possible, the downloaded update is kept for manual installation.
- Mod file version is shown alongside the patcher version in the Update section.

### Changed
- Mod files are managed as the supported/pinned bundle for this patcher build and are no longer auto-upgraded to newer upstream `dlssg_for_sm86` releases on startup (the supported bundle can still be ensured/repaired as needed).
- Improved Update and Graphics card status UI with progress bars during checks/refresh and clearer actions.

### Fixed
- **Open dir** now opens the game folder via Windows Explorer.

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
