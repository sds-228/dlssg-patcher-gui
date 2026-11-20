# Patcher Self-Update and Mod-Files Options

**Date:** 2026-11-20  
**Status:** Approved direction (design for implementation planning)  
**Scope:** `dlssg-patcher-gui` (single-file Windows GUI, Rust/eframe)

## Intent and why

`dlssg-patcher-gui` currently checks and downloads **`dlssg_for_sm86` mod releases** and treats that as the primary update flow. As the mod evolves (e.g. 0.4.1 → 0.4.2), new releases often introduce behavior/config changes that require patcher code changes and a new patcher build. Automatically fetching newer mod files no longer makes sense and produces confusing states (“mod has an update but patcher doesn’t support it yet” / “both mod and patcher have updates”).

This change makes **updating the patcher itself** the primary, seamless path to receive new mod support, and moves mod-file acquisition into an explicit, manual/advanced option.

## Goals

- Make patcher updates the main update mechanism: check this project’s GitHub releases, notify the user, and let the user choose to download and apply a patcher update.
- Stop automatically downloading/upgrading to newer upstream `dlssg_for_sm86` releases. Ensure the needed mod bundle is present/healthy without auto-upgrading to a newer upstream mod version on startup.
- Keep existing mod-file download/apply machinery available as a manual action in main **Options**, with a clear channel choice: **Default** vs **Unstable**.
- Remove/replace the current mod-upstream update card and its confusing messaging with patcher-update status/controls and mod-files Options.
- Preserve the one-click patch/patch-per-game-config behavior and existing per-game settings semantics, except where mod-file sourcing behavior intentionally changes.

## Non-goals

- Auto-downloading or silently installing patcher updates.
- Auto-upgrading mod files to newer upstream releases on startup (in any channel).
- Bundling mod payloads inside the patcher executable (unless separately decided later).
- Changing the per-game Config feature set as part of this work, beyond what is needed to reflect mod-file sourcing/status.
- Supporting in-place downgrade flows beyond normal “install/replace with a release build.”

## Anti-goals (would be a failure even if it “works”)

- Silently replacing the running patcher or mod files without the user explicitly requesting that action.
- In **Default** mode, pulling an upstream mod release that this patcher build is not intended to support.
- Losing or corrupting the user’s state (`state.json`, patch records, per-game settings/overrides) during a patcher update/apply.
- Making it unclear which mod bundle/channel is active or whether a downloaded mod bundle is supported vs unstable/unverified.

## Constraints

- **Single portable `.exe`:** self-update must work for a standalone Windows executable (no installer assumed).
- **Windows file-locking:** a running exe cannot be overwritten in place but can typically be renamed; self-apply must account for this and for non-writable install locations.
- **Release source:** patcher updates come from this repo’s GitHub releases (`sds-228/dlssg-patcher-gui`), assets produced by the existing release workflow (at least `dlssg-patcher.exe` / `dlssg-patcher-windows.zip`).
- **Upstream mod source:** mod files continue to come from upstream `sdli1995/dlssg_for_sm86` releases/zips as today, subject to channel/pinning rules below.
- **Integrity/trust:** existing SHA-256 pinning/verification behavior for known mod releases should be preserved where applicable; unpinned/unknown builds must be clearly labeled as unverified/unsupported as appropriate.
- **Network is best-effort:** update checks/downloads can fail (offline/rate limits); failures must be reported clearly without breaking core patch/unpatch use of already-available files.
- **Existing cache/state layout:** reuse/extend current cache and `state.json` conventions where practical; migrate state additively (old state must still load).

## Agreed behavior (the “what”)

### 1. Patcher self-update (primary)

- **Check:** on startup (background, non-blocking) and on user request, check the latest release of `sds-228/dlssg-patcher-gui` and compare against the running patcher version (`CARGO_PKG_VERSION`).
- **Notify only:** if an update is available, show a clear notification/status (e.g. “Patcher vX.Y.Z available”, current vs available). **Do not download automatically.**
- **User-initiated download:** provide an explicit action (e.g. “Download update” / “Update to vX.Y.Z”). Download the appropriate release asset in the background with progress/status. The download must be attributable to the selected release.
- **User-initiated apply (restart):** after a successful download, offer a clear “Restart to apply update” (or equivalent) action. The app does not replace itself silently; applying happens only when the user confirms/restarts to apply.
- **Apply mechanism (intended):** on Windows, stage the downloaded exe, then on user-confirmed restart perform a safe swap (e.g. rename running exe aside, place new exe at the original path, relaunch, and clean up old file on next start). If the app’s location is not writable or replacement cannot be completed safely, fall back gracefully: keep the downloaded file in a known location and tell the user where it is and how to replace it manually (and/or open its folder). Never leave the app in a broken half-updated state.
- **Messaging:** replace current mod-update-centric messaging. Upstream mod moving ahead without a corresponding patcher release may be surfaced only as secondary/advanced information if at all; the actionable update is the patcher update.

### 2. Mod files: no auto-upgrade, manual via Options

- **Startup:** do not auto-upgrade mod files to a newer upstream mod release. Ensure the needed mod bundle is present/healthy (repair missing/corrupt/incomplete payloads as appropriate) without upgrading to a newer upstream mod version on its own.
- **Mod files channel (persisted, user-selectable in main Options):**
  - **Default (recommended):** use the mod bundle this patcher version supports (the pinned/tested bundle for this build). Ensures/repairs that supported bundle if missing or corrupt; **never** auto-upgrades to a newer upstream mod release. Switching to/refreshing Default targets the supported bundle (not “latest upstream”).
  - **Unstable (advanced):** allow fetching the **latest upstream** `dlssg_for_sm86` mod files on explicit user action. Clearly labeled as possibly unsupported/unverified by this patcher. Integrity/trust indicators must reflect reality (e.g. unpinned/unverified when no known hash applies).
- **Manual action:** a “Refresh/Download mod files” (wording TBD) action in Options performs work **on demand** according to the selected channel. No automatic mod-file downloads/upgrades on startup in either channel.
- **Status:** Options show the current mod-files channel, the active/cached mod bundle version and health/trust state, and any relevant warnings (e.g. unstable/unsupported/unverified, or bundle missing).

### 3. UI / UX placement

- Replace the existing `dlssg_for_sm86` update card/flow with patcher-update status and controls plus an entry point to **main Options**.
- **Main Options** (modal/panel/section; placement is a builder choice consistent with the app’s style) contains: patcher update check/status (and download/apply affordances when applicable), mod-files channel selector (**Default / Unstable**), mod-files status, and manual mod-files refresh/download action.
- Per-game Config (gear) remains available as today, with its gating/messaging updated only insofar as it depends on mod bundle availability/support (not tied to upstream “latest”).

## Data and state (additive)

- Persist the selected mod-files channel in app state (e.g. `state.json`) with a safe default of **Default** for existing and new users.
- Existing state files lacking the new field(s) must continue to load (serde defaults / backward-compatible parsing).
- Downloaded patcher update artifacts (staged exe, metadata) may be stored under the app’s data/cache area; ephemeral per-session tracking is acceptable if persistence is not required for correctness.
- Continue recording mod version and related info as needed for patch records, status, and compatibility checks.

## Edge cases and failure handling

- **Offline / GitHub unavailable / rate-limited:** report failure; keep existing functionality usable with currently available mod files. Distinguish “couldn’t check” from “up to date”.
- **Asset missing or download interrupted/corrupt:** do not offer apply; report error and allow retry. Do not replace the current exe on failure.
- **Non-writable install path / replace failure:** do not break the running/current install; provide manual fallback (path to downloaded exe + instructions).
- **Channel switch:** switching channels changes what a manual refresh targets. The UI must make the active target clear; avoid surprising automatic network activity on switch alone (unless an explicit refresh is triggered).
- **Unstable bundle compatibility:** Unstable may fetch mod files this patcher doesn’t fully understand; operations that depend on unsupported features should fail or warn clearly rather than behave unpredictably. Default remains the supported path.
- **SR payload / existing download flows:** SR and other on-demand payload handling must remain consistent with the selected/active bundle version (no mixing bundle versions).

## Implementation notes (non-binding guidance for the builder)

- Reuse/refactor `src/download.rs` mod-fetch, caching, extraction, pinning/verification, and SR logic; separate concerns for (a) upstream mod bundle management and (b) patcher release checking/downloading.
- Add a small, well-scoped self-update module/helper for: release discovery, asset selection/download, staged apply-on-restart, and cleanup of previous-version leftovers.
- Keep version comparison and tag parsing robust (e.g. `vX.Y.Z` tags, semver-like crate version).
- Ensure update checks and downloads run off the UI thread with progress reporting, consistent with existing async/message patterns in `app.rs`.
- Update user-facing strings/help to reflect Default/Unstable meanings and the new update model.

## Out of scope follow-ups (separate decisions)

- Code-signing or cryptographic signature verification of the patcher exe beyond HTTPS/GitHub origin (can be added later).
- Auto-applying updates without a restart step.
- Telemetry/metrics.

## Acceptance criteria

- With no patcher update available, the app clearly shows patcher is up to date (or check status) and does not mention mod “latest” as an actionable auto-update.
- When a newer patcher release exists, the app notifies the user **without downloading automatically**; the user can download and then restart to apply it successfully (or receive a safe manual fallback if in-place apply is not possible).
- The app never auto-upgrades mod files to a newer upstream mod release on startup. In **Default**, manual refresh ensures/repairs the supported bundle and does not fetch a newer upstream mod release. In **Unstable**, manual refresh can fetch latest upstream mod files with appropriate warnings/labels.
- Mod-files channel (**Default/Unstable**) is available in main Options, persisted, and defaults to **Default**.
- Existing state/patch records load correctly after the change; patch/unpatch/repatch and per-game Config continue to function with the supported bundle.
- Errors (network, download, apply) are surfaced clearly and leave the app in a usable state.