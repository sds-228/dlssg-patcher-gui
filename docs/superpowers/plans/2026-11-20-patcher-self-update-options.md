# Patcher Self-Update and Mod-Files Options Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace automatic upstream mod-file updates with an opt-in patcher self-update flow (from this repo's GitHub releases) and move mod-file management into main Options with a persisted Default/Unstable channel.

**Architecture:** Split concerns in `src/download.rs`: keep upstream `dlssg_for_sm86` bundle fetch/cache logic but make it channel-aware (Default targets the build's supported/pinned bundle and never auto-upgrades to newer upstream releases; Unstable can fetch latest upstream on explicit action). Add a new `src/self_update.rs` module for patcher release discovery, exe download, and Windows apply-on-restart (stage + swap/relaunch with safe fallback). Rework `src/app.rs`/`src/manifest.rs` UI and state to surface patcher update status/controls and a main Options panel for mod-files channel/status/manual refresh, removing the old mod-upstream update card/messaging. Startup becomes "check patcher update (notify only) + ensure mod bundle healthy for selected channel without auto-upgrading upstream."

**Tech Stack:** Rust 2024, eframe/egui 0.32, serde/serde_json, ureq 3, sha2, std::process/Windows file operations, GitHub Releases API (`sds-228/dlssg-patcher-gui`).

**Spec:** `docs/superpowers/specs/2026-11-20-patcher-update-options-design.md`

## Global Constraints

- **No auto-download/install of patcher updates:** notify only on check; download and apply happen only on explicit user action.
- **No auto-upgrade of mod files to newer upstream releases on startup** in any channel.
- **Default channel:** uses the mod bundle this patcher build supports (pinned/tested). Manual refresh ensures/repairs that supported bundle and must not fetch a newer upstream mod release. Default is the safe persisted default for new and existing state.
- **Unstable channel:** manual refresh may fetch latest upstream `dlssg_for_sm86` mod files; clearly labeled unsupported/unverified when applicable. No automatic mod-file downloads/upgrades on startup.
- **Self-update source:** `https://api.github.com/repos/sds-228/dlssg-patcher-gui/releases/latest` and its release assets (`dlssg-patcher.exe` preferred). Apply must never leave a broken half-updated install; if in-place replacement is not possible, keep the downloaded exe and provide a manual fallback path.
- **Integrity:** preserve existing SHA-256 pinning/verification for known mod zips; unknown/unpinned bundles are accepted only as unverified and must be labeled as such.
- **State compatibility:** old `state.json` without new fields must load via serde defaults.
- **Network best-effort:** failures reported clearly; app remains usable with already-available files. Distinguish "couldn't check" from "up to date".
- **Preserve behavior:** one-click patch/unpatch/repatch, per-game Config and settings semantics, SR handling consistent with the active bundle, existing release asset names/workflow unchanged.

## Review Focus

- **Default selected + newer upstream mod release exists:** startup/refresh must not download or switch to the newer upstream mod; it must ensure/repair the build-supported bundle only.
- **Patcher update available:** UI shows an available update but performs no network download until the user clicks download; "Restart to apply" performs no replacement until explicitly confirmed.
- **Apply with non-writable exe location (e.g. Program Files):** replacement failure must not corrupt/remove the current exe; user gets a clear fallback with the staged file location.
- **Unstable channel fetch of unpinned/unverified upstream bundle:** bundle can be installed but status/warnings clearly mark it unverified/unsupported (no false "supported/pinned" claim).
- **Offline/rate-limited on startup:** check failures are non-fatal, mod files remain usable if cached, and status distinguishes check failure from up-to-date/no-update.

---

## File Structure

- **Create:** `src/self_update.rs` — patcher release model/check, asset download, staged-apply-on-restart and startup cleanup helpers for Windows single-exe updates. Owns only patcher-app update concerns (no mod-bundle logic).
- **Modify:** `src/download.rs` — add `ModChannel`/supported-bundle targeting and ensure logic for upstream mod files; separate/refactor so "ensure supported bundle (Default)" vs "fetch latest upstream (Unstable, explicit)" are explicit; keep SR/version/hash helpers reusable.
- **Modify:** `src/manifest.rs` — persist `mod_files_channel` on `State` with backward-compatible defaults.
- **Modify:** `src/app.rs` — app state/messages for patcher update check/download/apply and mod-channel/options; replace `update_card`/mod-upstream check-refresh flow with patcher-update + Options UI; adjust startup `refresh_files`/checks to match spec.
- **Modify:** `src/main.rs` — register `mod self_update;` and perform early startup cleanup of leftover self-update temp/old exe from a previous apply attempt (best-effort, before normal init).

Responsibilities stay separated: mod bundle acquisition/verification in `download.rs`, app self-replacement mechanics in `self_update.rs`, persisted preferences in `manifest.rs`, orchestration/UI in `app.rs`.

## Implementation Tasks

### Task 1: Persist mod-files channel in app state

**Files:**
- Modify: `src/manifest.rs`
- Test: `src/manifest.rs` (inline `#[cfg(test)]` module)

**Interfaces:**
- Produces: `pub enum ModChannel { Default, Unstable }` with `Default` as the default; `impl Default for ModChannel` returns `Default`. Serde-friendly (e.g. `#[derive(..., Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]` with `#[serde(rename_all = "lowercase")]` or explicit names) — stored value must round-trip.
- Produces: `State.mod_files_channel: ModChannel` annotated `#[serde(default)]` so missing field deserializes to `Default`.
- Produces: `impl State { pub fn mod_files_channel(&self) -> ModChannel { self.mod_files_channel } }` (or direct field access; pick one and use consistently).

- [ ] **Step 1: Write failing tests for channel defaulting and round-trip**
  Add tests in `manifest::tests`:
  - old state json without `mod_files_channel` deserializes with `state.mod_files_channel == ModChannel::Default`
  - serializing state with `ModChannel::Unstable` and deserializing back preserves `Unstable`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -q manifest 2>&1 | Select-Object -Last 20`
Expected: FAIL (missing `ModChannel`/field)

- [ ] **Step 3: Implement `ModChannel` and `State.mod_files_channel` with serde defaults**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -q 2>&1 | Select-Object -Last 12`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/manifest.rs
git commit -m "feat: persist mod files channel in app state"
```

### Task 2: Channel-aware mod bundle targeting in download.rs

**Files:**
- Modify: `src/download.rs`

**Interfaces:**
- Produces: `pub const SUPPORTED_MOD_VERSION: &str = "0.4.2";` (the bundle this build supports; must be pinned/known).
- Produces: `pub fn supported_mod_version() -> &'static str` (returns `SUPPORTED_MOD_VERSION`).
- Produces/changes: mod ensure entrypoint(s) that can (a) ensure the **supported** bundle present/healthy without fetching a newer upstream release, and (b) fetch **latest upstream** bundle on explicit request (Unstable). Keep `ensure_mod_files` usable by refactoring internally or introduce a channel/mode-aware function; mode must express: `DefaultEnsure` (repair/ensure supported, no upgrade to newer upstream) and `UnstableFetchLatest` (explicit refresh fetches latest upstream). `force` semantics preserved where sensible (re-fetch target even if present).
- Produces: helpers the UI/app can use to know whether cached bundle is the supported one / pinned/unverified for its version (`is_pinned_version` exists).
- Consumes: existing `PINNED_RELEASE_SHA256`, zip fetch/extract/verify, cache layout, `cached_mod_version`, `cache_is_complete`.

- [ ] **Step 1: Write failing tests for supported-vs-latest targeting**
  Add/adjust tests to assert: supported version constant is pinned (`is_pinned_version(SUPPORTED_MOD_VERSION)`); a helper/select logic for Default targets supported (not latest); cache-complete short-circuit for Default does not fetch a newer upstream tag when cache already matches supported. Prefer pure/unit-testable selection logic (no network).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -q 2>&1 | Select-Object -Last 20`
Expected: FAIL

- [ ] **Step 3: Implement supported-bundle targeting and channel-aware ensure**
  - Default ensure: resolve/download only the supported bundle version/tag/assets (use `zip_urls`/pinned release for `SUPPORTED_MOD_VERSION`), never `/releases/latest` as an upgrade target; if cache is complete and `cached_mod_version == SUPPORTED_MOD_VERSION` and not forced, return cached.
  - Unstable fetch-latest (explicit): allow latest-upstream resolution/download; result may be unverified if unpinned.
  - Preserve SR behavior pinned to cached bundle version.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -q 2>&1 | Select-Object -Last 12`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/download.rs
git commit -m "feat: make mod bundle fetching channel-aware"
```

### Task 3: Patcher self-update check and download module

**Files:**
- Create: `src/self_update.rs`
- Modify: `src/main.rs` (declare module)
- Modify: `Cargo.toml` only if a dependency is truly required (prefer existing deps)

**Interfaces:**
- Produces: `pub struct PatcherRelease { pub version: String, pub tag: String, pub exe_url: String, pub asset_name: String }`
- Produces: `pub enum PatcherUpdateStatus { UpToDate { current: String }, Available(PatcherRelease), Error(String) }` (or equivalent consumed by app)
- Produces: `pub fn check_patcher_update() -> Result<PatcherUpdateStatus, String>` — GET `https://api.github.com/repos/sds-228/dlssg-patcher-gui/releases/latest`, pick `dlssg-patcher.exe` asset, compare tag/version vs `env!("CARGO_PKG_VERSION")`. Network errors return `Err`.
- Produces: `pub fn download_patcher_update(release: &PatcherRelease, dest: &Path, progress: Option<Box<dyn Fn(String)+Send>>) -> Result<PathBuf, String>` — downloads to `dest` (atomic temp+rename in same dir), returns final path. No apply.
- Produces: staging-path helper under app data/cache, e.g. `pub fn staged_update_path(cache_root: &Path, version: &str) -> PathBuf`.

- [ ] **Step 1: Write failing tests for version/tag parsing and asset selection**
  Unit-test helpers: normalize tag `v2.0.3` -> `2.0.3`, compare versions, select `dlssg-patcher.exe` asset from sample release JSON.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -q 2>&1 | Select-Object -Last 20`
Expected: FAIL

- [ ] **Step 3: Implement check/download helpers and module registration**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -q 2>&1 | Select-Object -Last 12`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/self_update.rs src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: add patcher update check and download helpers"
```

### Task 4: Apply-on-restart and startup cleanup (Windows-safe)

**Files:**
- Modify: `src/self_update.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `pub fn apply_update_on_restart(staged_exe: &Path) -> Result<(), String>` — prepares and triggers restart swap. Determine current exe path; if not writable/replaceable return actionable error (caller falls back to manual). Otherwise rename running exe to a sibling `.old`, move/copy staged exe to current exe path, spawn the new exe detached, and request app exit. Must not corrupt current install on failure.
- Produces: `pub fn cleanup_update_leftovers()` — best-effort removal of `.old`/temp updater artifacts on startup (ignore errors).

- [ ] **Step 1: Write failing tests for pure path/decision helpers**
  Test helper that computes backup/staged paths and fallback conditions from inputs (no privileged fs ops).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -q 2>&1 | Select-Object -Last 20`
Expected: FAIL

- [ ] **Step 3: Implement apply and cleanup helpers; call `cleanup_update_leftovers()` early in `main`**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -q 2>&1 | Select-Object -Last 12`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/self_update.rs src/main.rs
git commit -m "feat: apply patcher updates on restart"
```

### Task 5: App state, messages, and startup behavior

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Replace mod-upstream update state (`UpdateInfo`/`Msg::Update`/`check_for_updates`) with patcher-update state: check result, staged/downloaded release path, in-progress/error flags; messages like `PatcherUpdateChecked(...)`, `PatcherUpdateDownloaded(...)` plus progress reuse.
- `refresh_files`/ensure becomes channel-aware: Default ensures supported bundle (no newer-upstream upgrade), Unstable manual refresh fetches latest upstream. Startup does a non-upgrading ensure appropriate for channel and a patcher update check (notify only).
- Config/feature gating that referenced mod "update to vX" now points to patcher update / Options mod-files refresh as appropriate, without mod auto-upgrade.
- `row_needs_update` and SR flows continue to work against cached bundle version.

- [ ] **Step 1: Implement state/message refactor and channel-aware ensure/check wiring**

- [ ] **Step 2: Update/adjust unit tests in `app.rs` affected by changed helpers/signatures**

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -q 2>&1 | Select-Object -Last 12`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "feat: wire patcher update checks and channel-aware mod files"
```

### Task 6: Options and update UI

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Replace `update_card` (dlssg_for_sm86 upstream) with patcher update status UI and an **Options** entry point (button/panel/modal consistent with existing modals). Options contains: patcher update section (status, "Check for updates", "Download update" when available, "Restart to apply" when downloaded, progress/errors, manual fallback info) and mod files section (channel selector Default/Unstable, status/version/trust/health, "Refresh/Download mod files" acting on selected channel). All actions explicit; no auto-download.
- Header/version indicator reflects patcher update availability (not upstream mod).
- Labels: Default = supported bundle for this patcher; Unstable = latest upstream, may be unsupported/unverified.

- [ ] **Step 1: Implement Options UI and replace update card/header indicators**

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -q 2>&1 | Select-Object -Last 12`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/app.rs
git commit -m "feat: add options panel for patcher updates and mod channels"
```

### Task 7: Verification and local release build

**Files:**
- Modify (only if needed): `docs/superpowers/specs/2026-11-20-patcher-update-options-design.md`
- Modify (optional): `CHANGELOG.md` — add notes if helpful; do not bump version/tag unless requested.

- [ ] **Step 1: Run full test suite**

Run: `cargo test -q 2>&1 | Select-Object -Last 15`
Expected: PASS (all existing + new tests)

- [ ] **Step 2: Build local release binary for user testing**

Run: `cargo build --release 2>&1 | Select-Object -Last 12`
Expected: build succeeds, exe at `target\release\dlssg-patcher.exe`

- [ ] **Step 3: Sanity-check acceptance scenarios against spec (notify-only, no mod auto-upgrade, Default targets supported, manual fallback path on apply failure)**

- [ ] **Step 4: Commit any final fixes**

```bash
git add -A
git commit -m "chore: finalize patcher update and mod options implementation"
```

## Notes for Implementer

- Prefer small, reviewable commits per task. Keep UI strings explicit about risk (Unstable/unverified).
- When refactoring `ensure_mod_files`, preserve SR helpers (`ensure_sr_files` pinned to cached bundle version) and integrity verification.
- Be careful with Windows rename semantics in `apply_update_on_restart`: testable pure helpers first, best-effort fs ops guarded with clear errors.
- Do not guess release asset names; prefer exact `dlssg-patcher.exe` match and tolerate absence with a clear error.
- Bump `Cargo.toml`/`Cargo.lock`/`CHANGELOG.md` and tag a release only when the user asks to cut a release.
