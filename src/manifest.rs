use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const STATE_FOLDER: &str = "dlssg-patcher";

/// Built-in database of games NVIDIA lists as shipping DLSS Frame Generation.
///
/// This answers exactly one question: does the game have Frame Generation?
/// That is the prerequisite the mod needs. It does NOT mean the patch will work,
/// which also depends on which proxy DLL the game loads -- no public list covers
/// that. Games are matched to a Steam install by AppID where known, falling back
/// to folder name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub games: Vec<ManifestGame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestGame {
    /// User-facing display name.
    pub name: String,
    /// Steam AppID, the reliable key: an install's AppID never changes, while
    /// folder names can be renamed by the user.
    #[serde(default)]
    pub steam_appid: Option<u32>,
    /// Whether NVIDIA lists native Frame Generation ("yes") or native FG with
    /// NVIDIA App override support ("override").
    #[serde(default)]
    pub fg: Option<String>,
    /// How `steam_appid` was resolved ("exact", "qualifier", "fuzzy"). Shown in
    /// the tooltip so a shaky match is never presented as certainty.
    #[serde(default, rename = "match")]
    pub match_confidence: Option<String>,
    /// Steam `steamapps/common/<folder>` folder names that identify this game.
    /// Optional; a renamed folder falls back to the AppID match.
    #[serde(default)]
    pub folders: Vec<String>,
    /// Path of the actual rendering exe, relative to the game install dir.
    /// Optional: most entries have no curated path and are autodetected instead.
    #[serde(default)]
    pub render_exe: String,
    /// Which proxy DLL name to install (version.dll, winmm.dll, dinput8.dll, winhttp.dll, dxgi.dll).
    #[serde(default = "default_proxy")]
    pub proxy: String,
}

impl ManifestGame {
    /// Whether the resolved AppID is a confident match.
    /// A "fuzzy" match is a guess, so the UI says so rather than implying certainty.
    pub fn confident_match(&self) -> bool {
        !matches!(self.match_confidence.as_deref(), Some("fuzzy") | Some("none"))
    }
}

fn default_proxy() -> String {
    "version.dll".to_string()
}

const EMBEDDED_MANIFEST: &str = include_str!("../games.json");

/// Load the manifest: prefer an editable `games.json` next to the executable,
/// otherwise fall back to the embedded copy.
pub fn load_manifest() -> Result<Manifest, String> {
    let mut manifest: Manifest = serde_json::from_str(EMBEDDED_MANIFEST)
        .map_err(|e| format!("failed to parse embedded games.json: {e}"))?;

    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())) {
        let external = exe_dir.join("games.json");
        if external.is_file() {
            match serde_json::from_str::<Manifest>(&std::fs::read_to_string(&external).map_err(|e| e.to_string())?) {
                Ok(m) => manifest = m,
                Err(e) => eprintln!("[dlssg-patcher] ignoring bad external games.json ({}): {e}", external.display()),
            }
        }
    }
    Ok(manifest)
}

/// Look up a catalog entry for a scanned install.
///
/// AppID wins when we have one: it is stable, whereas a folder name can be
/// edited by the user or differ between regions. Folder/name matching stays as
/// a fallback for entries with no AppID and for non-Steam installs.
pub fn find_manifest_game<'a>(manifest: &'a Manifest, folder_name: &str) -> Option<&'a ManifestGame> {
    let key = folder_name.to_lowercase();
    manifest.games.iter().find(|g| {
        g.folders.iter().any(|f| f.to_lowercase() == key) || g.name.to_lowercase() == key
    })
}

/// Same as [`find_manifest_game`] but prefers a Steam AppID match.
pub fn find_manifest_game_by_appid<'a>(
    manifest: &'a Manifest,
    appid: Option<u32>,
    folder_name: &str,
) -> Option<&'a ManifestGame> {
    if let Some(id) = appid {
        if let Some(g) = manifest.games.iter().find(|g| g.steam_appid == Some(id)) {
            return Some(g);
        }
    }
    find_manifest_game(manifest, folder_name)
}

/// Persistent app state: which games are patched, plus manually added games.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default)]
    pub patches: Vec<PatchRecord>,
    #[serde(default)]
    pub manual_games: Vec<ManualGame>,
    /// Global default proxy DLL, applied to every game with no per-game override and no
    /// catalogue entry of its own. Empty or unrecognised means `version.dll`.
    #[serde(default)]
    pub default_proxy: String,
    /// Per-game overrides, keyed by lower-cased install directory (not the render exe:
    /// a row can exist before its exe is located, and `Locate`/`Relink` replace the exe
    /// while the folder stays meaningful).
    #[serde(default)]
    pub proxy_overrides: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchRecord {
    pub game_name: String,
    pub install_dir: String,
    pub render_exe: String,
    pub proxy_dll: String,
    pub router: String,
    pub installed_files: Vec<String>,
    pub backed_up: Vec<BackedUpFile>,
    pub installed_at_unix: u64,
    pub proxy_sha256: String,
    /// dlssg_for_sm86 version that was used at patch time (e.g. "0.2.4").
    #[serde(default)]
    pub mod_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackedUpFile {
    /// Original location inside the game directory.
    pub original_path: String,
    /// Where the original was moved to.
    pub backup_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualGame {
    pub name: String,
    pub install_dir: String,
    pub render_exe: String,
    pub proxy: String,
}

/// Canonical lookup key for a render-exe path: lower-cased for case-insensitive
/// comparison on Windows. Every comparison between a stored record and a freshly
/// scanned path goes through this.
pub fn exe_key(exe: &str) -> String {
    exe.to_lowercase()
}

impl State {
    pub fn patch_for_exe(&self, exe: &str) -> Option<&PatchRecord> {
        let key = exe_key(exe);
        self.patches.iter().find(|p| exe_key(&p.render_exe) == key)
    }

    /// Insert or replace the record for `record.render_exe`, keyed with
    /// `exe_key` so a re-patch always resolves the existing entry regardless of
    /// path casing. The `backed_up` list is owned by `install`: each new record
    /// carries the backups `install` could still prove are game originals, so a
    /// re-patch neither loses a genuine backup nor re-restores one over files it
    /// just wrote.
    pub fn upsert_patch(&mut self, record: PatchRecord) {
        let key = exe_key(&record.render_exe);
        match self.patches.iter().position(|p| exe_key(&p.render_exe) == key) {
            Some(i) => self.patches[i] = record,
            None => self.patches.push(record),
        }
    }

    /// The global default proxy, never an empty or unusable value.
    ///
    /// Case-insensitive, and it returns the canonical spelling: these are Windows filenames, so a
    /// hand-edited `state.json` may hold `WINMM.DLL`, and the tooltip, the combo and the file `install`
    /// writes must all agree on one name.
    pub fn effective_default_proxy(&self) -> &str {
        match crate::download::PROXY_NAMES
            .iter()
            .find(|n| n.eq_ignore_ascii_case(&self.default_proxy))
        {
            Some(name) => *name,
            None => "version.dll",
        }
    }

    /// Record a per-game proxy choice. Keyed by install dir (see the field docs).
    pub fn set_proxy_override(&mut self, install_dir: &str, proxy: String) {
        self.proxy_overrides.insert(exe_key(install_dir), proxy);
    }

    pub fn proxy_override_for(&self, install_dir: &str) -> Option<&str> {
        self.proxy_overrides.get(&exe_key(install_dir)).map(|s| s.as_str())
    }
}

pub fn state_dir() -> Result<PathBuf, String> {
    let base = dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .ok_or_else(|| "could not locate the local app data directory".to_string())?;
    Ok(base.join(STATE_FOLDER))
}

pub fn load_state() -> Result<State, String> {
    let path = state_dir()?.join("state.json");
    if !path.is_file() {
        return Ok(State::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

pub fn save_state(state: &State) -> Result<(), String> {
    let dir = state_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join("state.json");
    let tmp = dir.join("state.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state).map_err(|e| e.to_string())?)
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Per-install backup folder name: `<slug>-<8-hex-char sha256 of exe_key(render_exe)>`.
/// Disambiguates two installs that share a display name (e.g. Steam + Epic) or
/// non-ASCII titles whose ASCII slug collapses to `"game"`.
pub fn backup_folder_name(game_name: &str, render_exe: &Path) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let key = exe_key(&render_exe.to_string_lossy());
    let digest = Sha256::digest(key.as_bytes());
    let mut short_hash = String::with_capacity(8);
    for b in &digest[..4] {
        let _ = write!(short_hash, "{b:02x}");
    }
    format!("{}-{short_hash}", slugify(game_name))
}

/// Turn a game name into a filesystem-safe slug used for backup folders.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    if t.is_empty() {
        "game".to_string()
    } else {
        t.to_string()
    }
}

pub fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    use std::fmt::Write as _;
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

pub fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(exe: &str) -> PatchRecord {
        PatchRecord {
            game_name: "Game".into(),
            install_dir: r"C:\games\Game".into(),
            render_exe: exe.into(),
            proxy_dll: "version.dll".into(),
            router: "SM86".into(),
            installed_files: vec![],
            backed_up: vec![],
            installed_at_unix: 0,
            proxy_sha256: "abc".into(),
            mod_version: None,
        }
    }

    /// Windows paths are case-insensitive, and a rescan can easily come back with a
    /// different drive-letter or folder casing than patch time. The record lookup
    /// must agree with the on-disk cache key, otherwise a patched game looks stale.
    #[test]
    fn patch_for_exe_ignores_path_case() {
        let mut state = State::default();
        state.patches.push(record(r"d:\Steam\steamapps\common\Game\Game.exe"));
        assert!(
            state.patch_for_exe(r"D:\Steam\steamapps\common\Game\Game.exe").is_some(),
            "drive-letter case must not matter"
        );
        assert!(
            state.patch_for_exe(r"D:\steam\STEAMAPPS\COMMON\game\GAME.EXE").is_some(),
            "folder and file casing must not matter"
        );
        assert!(state.patch_for_exe(r"D:\Other\Game.exe").is_none(), "unrelated path must not match");
    }

    #[test]
    fn exe_key_normalizes_case() {
        assert_eq!(exe_key(r"D:\Games\A.exe"), exe_key(r"d:\games\a.exe"));
    }

    /// Patch and Unpatch both resolve stale state by (re)recording the current
    /// truth for an exe. Upsert must replace the existing record for that exe -
    /// matching case-insensitively like every other lookup - and append when
    /// there is none.
    #[test]
    fn upsert_patch_replaces_existing_and_appends_new() {
        let mut state = State::default();
        let mut first = record(r"D:\Games\A\A.exe");
        first.proxy_sha256 = "old".into();
        first.backed_up.push(BackedUpFile {
            original_path: r"D:\Games\A\version.dll".into(),
            backup_path: r"C:\backup\version.dll".into(),
        });
        state.upsert_patch(first);

        let mut second = record(r"E:\Games\B\B.exe");
        second.proxy_sha256 = "b".into();
        state.upsert_patch(second);
        assert_eq!(state.patches.len(), 2, "a different exe must append");

        // Re-patch the first game with different drive-letter casing; it must
        // replace in place (same slot), not duplicate, and drop the stale backup
        // list (a fresh Patch makes its own backups).
        let mut repatch = record(r"d:\games\a\A.exe");
        repatch.proxy_sha256 = "new".into();
        state.upsert_patch(repatch);

        assert_eq!(state.patches.len(), 2, "re-patching must not duplicate the record");
        let r = state.patch_for_exe(r"D:\Games\A\A.exe").expect("record present");
        assert_eq!(r.proxy_sha256, "new", "the new record must win");
        assert!(r.backed_up.is_empty(), "the replaced record's backups are dropped");
        // Position is preserved so the UI row order is stable across a re-patch.
        assert_eq!(state.patches[0].render_exe, r"d:\games\a\A.exe");
    }

    #[test]
    fn default_proxy_falls_back_to_version_dll() {
        let mut s = State::default();
        assert_eq!(s.effective_default_proxy(), "version.dll", "empty is not a proxy name");
        s.default_proxy = "winmm.dll".into();
        assert_eq!(s.effective_default_proxy(), "winmm.dll");
        s.default_proxy = "winhttp.dll".into(); // not bundled
        assert_eq!(s.effective_default_proxy(), "version.dll");
        s.default_proxy = "WINMM.DLL".into(); // Windows filenames are case-insensitive...
        assert_eq!(s.effective_default_proxy(), "winmm.dll"); // ...and the canonical spelling wins
    }

    #[test]
    fn proxy_overrides_are_keyed_by_install_dir_case_insensitively() {
        let mut s = State::default();
        s.set_proxy_override(r"H:\Steam\Game", "dinput8.dll".into());
        assert_eq!(s.proxy_override_for(r"h:\steam\game"), Some("dinput8.dll"));
        assert_eq!(s.proxy_override_for(r"H:\Steam\Other"), None);
    }

    /// A fresh, uniquely named temp directory for the persisted-state fixture. The prefix
    /// deliberately differs from `download.rs`'s `dl_test_*` fixtures: libtest runs one
    /// binary's tests as threads in a single process, so sharing a path would be a race.
    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("manifest_test_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An older `state.json` predating `default_proxy`, `proxy_overrides` and `mod_version`
    /// must still deserialize, keep every field it does carry, and round-trip through save
    /// without losing anything. The fixture lives in a temp dir.
    #[test]
    fn old_state_json_without_new_keys_still_loads_and_round_trips() {
        // No `default_proxy`, no `proxy_overrides`, and a record without `mod_version`.
        let old = r#"{
            "patches": [{
                "game_name": "Cyberpunk 2077",
                "install_dir": "D:\\Steam\\Cyberpunk 2077",
                "render_exe": "D:\\Steam\\Cyberpunk 2077\\bin\\x64\\Cyberpunk2077.exe",
                "proxy_dll": "version.dll",
                "router": "SM86",
                "installed_files": ["version.dll", "dlssg_sm86.ini"],
                "backed_up": [],
                "installed_at_unix": 1700000000,
                "proxy_sha256": "deadbeef"
            }],
            "manual_games": [{
                "name": "Manual Game",
                "install_dir": "E:\\Games\\Manual",
                "render_exe": "E:\\Games\\Manual\\game.exe",
                "proxy": "winmm.dll"
            }]
        }"#;

        let dir = tmp_dir("old_state");
        let path = dir.join("state.json");
        std::fs::write(&path, old).unwrap();

        // Load exactly the way `load_state` does, but from the fixture.
        let state: State = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .expect("an old state.json must still deserialize");

        // The keys the blob carried survive...
        assert_eq!(state.patches.len(), 1);
        assert_eq!(state.patches[0].game_name, "Cyberpunk 2077");
        assert_eq!(state.patches[0].installed_files.len(), 2);
        assert!(state.patches[0].mod_version.is_none(), "a missing mod_version defaults to None");
        assert_eq!(state.manual_games.len(), 1);
        assert_eq!(state.manual_games[0].proxy, "winmm.dll");
        // ...and the new fields default instead of failing the parse.
        assert_eq!(state.effective_default_proxy(), "version.dll");
        assert_eq!(state.proxy_override_for(r"E:\Games\Manual"), None);

        // Saving back must not drop the existing data.
        let saved = serde_json::to_string_pretty(&state).unwrap();
        let reloaded: State = serde_json::from_str(&saved).expect("our own output must reload");
        assert_eq!(reloaded.patches.len(), 1);
        assert_eq!(reloaded.patches[0].proxy_sha256, "deadbeef");
        assert_eq!(reloaded.manual_games.len(), 1);
        assert_eq!(reloaded.manual_games[0].install_dir, r"E:\Games\Manual");
        assert_eq!(reloaded.effective_default_proxy(), "version.dll");

        // A minimal shape with only the required keys loads too.
        let minimal: State =
            serde_json::from_str("{\"patches\":[],\"manual_games\":[]}").expect("minimal old blob");
        assert_eq!(minimal.effective_default_proxy(), "version.dll");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn entry(name: &str, appid: Option<u32>, conf: Option<&str>) -> ManifestGame {
        ManifestGame {
            name: name.into(),
            fg: Some("yes".into()),
            steam_appid: appid,
            match_confidence: conf.map(|s| s.into()),
            folders: vec![],
            render_exe: String::new(),
            proxy: default_proxy(),
        }
    }

    #[test]
    fn appid_match_beats_folder_name() {
        let m = Manifest {
            games: vec![entry("Cyberpunk 2077", Some(1091500), Some("exact"))],
        };
        // The folder was renamed, so only the AppID can identify this install.
        let g = find_manifest_game_by_appid(&m, Some(1091500), "SomeRenamedFolder");
        assert_eq!(g.map(|g| g.name.as_str()), Some("Cyberpunk 2077"));
        // An AppID that is absent from the catalog still falls back to the
        // folder/name, so catalog entries with no resolved AppID can still match.
        assert!(find_manifest_game_by_appid(&m, Some(999), "Cyberpunk 2077").is_some());
        assert!(find_manifest_game_by_appid(&m, Some(999), "Unrelated Folder").is_none());
        // No AppID (Epic install, manual entry) still falls back to the name.
        assert!(find_manifest_game_by_appid(&m, None, "Cyberpunk 2077").is_some());
    }

    #[test]
    fn fuzzy_matches_are_flagged_as_unconfident() {
        assert!(entry("X", Some(1), Some("exact")).confident_match());
        assert!(entry("X", Some(1), Some("qualifier")).confident_match());
        assert!(!entry("X", Some(1), Some("fuzzy")).confident_match());
        assert!(!entry("X", Some(1), Some("none")).confident_match());
        // An entry with no AppID at all is not an AppID claim to be unsure of.
        assert!(entry("X", None, None).confident_match());
    }

    /// The catalog is generated, so a schema drift between the generator and
    /// this struct would break every lookup at runtime rather than at compile
    /// time. Parsing the embedded copy here makes that a test failure.
    #[test]
    fn embedded_catalog_parses_and_has_appids() {
        let m: Manifest = serde_json::from_str(EMBEDDED_MANIFEST)
            .expect("embedded games.json must parse");
        assert!(m.games.len() > 300, "expected a full catalog, got {}", m.games.len());
        assert!(
            m.games.iter().filter(|g| g.steam_appid.is_some()).count() > 300,
            "most entries should carry an AppID"
        );
        let cyberpunk = find_manifest_game_by_appid(&m, Some(1091500), "whatever");
        assert_eq!(cyberpunk.map(|g| g.name.as_str()), Some("Cyberpunk 2077"));
        // render_exe is absent from generated entries, so it must default rather
        // than fail deserialization.
        assert!(m.games.iter().all(|g| g.render_exe.is_empty()));
    }
}
