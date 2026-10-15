use std::path::{Path, PathBuf};
use std::os::windows::process::CommandExt;

use crate::anticheat::AcScan;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Parse `reg query HKCU\Software\Valve\Steam /v SteamPath` output. The value
/// line is `    SteamPath    REG_SZ    <path>` (or `REG_EXPAND_SZ`), and the path
/// may contain spaces. Returns `Some` only when the folder actually exists.
pub(crate) fn parse_steam_path(reg_output: &str) -> Option<PathBuf> {
    for line in reg_output.lines() {
        let line = line.trim();
        if !line.to_ascii_lowercase().starts_with("steampath") {
            continue;
        }
        let Some(kind) = line.split_whitespace().find(|t| t.starts_with("REG_")) else {
            continue;
        };
        let Some(value) = line.split_once(kind).map(|(_, v)| v.trim()) else {
            continue;
        };
        if value.is_empty() {
            return None;
        }
        let p = PathBuf::from(value.replace('/', "\\"));
        return p.is_dir().then_some(p);
    }
    None
}

/// Resolve the Steam installation path from the registry.
/// Avoids extra crates by shelling out to `reg.exe`.
pub fn steam_path_from_registry() -> Option<PathBuf> {
    let out = std::process::Command::new("reg")
        .args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_steam_path(&String::from_utf8_lossy(&out.stdout))
}

#[derive(Debug, Clone)]
pub struct InstalledGame {
    /// Name of the folder inside `steamapps/common`.
    pub folder_name: String,
    /// Absolute path to the game install directory.
    pub install_dir: PathBuf,
    /// Best-guess render executable found by scanning the install dir (may be None).
    pub detected_exe: Option<PathBuf>,
    /// Which storefront/launcher this game was found through ("Steam", "Epic", …).
    pub source: String,
    /// Steam AppID from `steamapps/appmanifest_<id>.acf`, when the install has one.
    /// This is the only reliable link between an installed folder and a catalog
    /// entry, because folder names can be renamed.
    pub appid: Option<u32>,
    /// Anti-cheat scan result for the install folder (marker + patch gate).
    pub anti_cheat: AcScan,
}

/// Subpaths to probe quickly before falling back to a full walk.
/// These cover the common per-engine layout for Windows titles.
const PROBE_SUBDIRS: &[&str] = &[
    r"Bin\Win64",
    r"Binaries\Win64",
    r"Binaries\Win32",
    r"Bin\Win32",
    r"Shipping\Win64",
    r"Win64",
    r"Bin\X64",
    r"Binaries\X64",
    r"Bin",
    r"Binaries",
    r"Shipping",
    r"Retail",
    r"Game\Binaries\Win64",
    r"Game\Binaries",
    r"Engine\Binaries\Win64",
    r"Engine\Binaries",
];

/// Directory names we never expect to hold a render executable.
const SKIP_DIRS: &[&str] = &[
    "content",
    "assets",
    "asset",
    "data",
    "saves",
    "mods",
    "redist",
    "commonredist",
    "engine",
    ".git",
    "config",
];

/// Best-effort heuristic to locate a game's render executable without a catalog entry.
/// Probes a few well-known engine subpaths first; only if that finds nothing does it
/// walk the tree (bounded depth + entry cap), so scanning hundreds of games stays fast.
pub fn autodetect_render_exe(install_dir: &Path) -> Option<PathBuf> {
    use std::collections::VecDeque;

    let root_lower = install_dir
        .file_name()
        .map(|f| f.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    // Fast path: probe known subpaths.
    for sub in PROBE_SUBDIRS {
        let dir = install_dir.join(sub);
        if let Some(exe) = best_exe_in(&dir, install_dir, &root_lower) {
            if score_exe_relative(&exe, install_dir, &root_lower) >= PROBE_STRONG {
                return Some(exe);
            }
        }
    }

    // Fallback: bounded BFS over the whole tree.
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((install_dir.to_path_buf(), 0));
    bfs_best(&mut queue, install_dir, &root_lower, MAX_EXPLORE)
}

/// True when `install_dir` contains at least one `.exe` anywhere (bounded walk).
/// Tells a real-but-unlocatable install apart from an empty/orphan folder left
/// behind by an uninstalled game, so the latter can be dropped from the list.
pub fn has_any_exe(install_dir: &Path) -> bool {
    const CAP: usize = 8000;
    const DEPTH: usize = 16;
    let mut stack = vec![(install_dir.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if visited >= CAP {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            visited += 1;
            if visited >= CAP {
                break;
            }
            let Ok(ftype) = entry.file_type() else { continue };
            let path = entry.path();
            if ftype.is_dir() {
                if depth < DEPTH && !path.is_symlink() {
                    stack.push((path, depth + 1));
                }
            } else if ftype.is_file()
                && path.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

/// Bounds for the fallback tree walk. Generous enough to reach render exes in
/// large Unreal installs (deep `Engine\Binaries\…` layouts) while still keeping a
/// full scan of hundreds of games down to seconds - it runs off the UI thread.
const MAX_EXPLORE: usize = 1600;
const FN_EXPLORE: usize = 400;
const MAX_DEPTH: usize = 9;
const PROBE_STRONG: i32 = 50;

fn best_exe_in(dir: &Path, root: &Path, root_lower: &str) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    let mut best_score = i32::MIN;
    let entries = std::fs::read_dir(dir).ok()?;
    for (n, entry) in entries.flatten().enumerate() {
        if n > FN_EXPLORE {
            break;
        }
        let Ok(ftype) = entry.file_type() else { continue };
        if !ftype.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false) {
            let s = score_exe_relative(&path, root, root_lower);
            if s > best_score {
                best_score = s;
                best = Some(path);
            }
        }
    }
    best
}

fn should_skip(dir_name: &str) -> bool {
    let d = dir_name.to_lowercase();
    SKIP_DIRS.iter().any(|k| d == *k)
}

fn bfs_best(
    queue: &mut std::collections::VecDeque<(PathBuf, usize)>,
    install_root: &Path,
    root_lower: &str,
    cap: usize,
) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    let mut best_score = i32::MIN;
    let mut visited = 0usize;

    'search: while let Some((dir, depth)) = queue.pop_front() {
        if depth > MAX_DEPTH {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if visited >= cap {
                break 'search;
            }
            visited += 1;
            let path = entry.path();
            let Ok(ftype) = entry.file_type() else { continue };
            if ftype.is_dir() {
                if depth < MAX_DEPTH && !path.is_symlink() && !should_skip(
                    &path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default(),
                ) {
                    queue.push_back((path, depth + 1));
                }
            } else if ftype.is_file() && path.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false) {
                let score = score_exe_relative(&path, install_root, root_lower);
                let cur_size = exe_size_mb(&path);
                let best_size = best.as_ref().map(|p| exe_size_mb(p)).unwrap_or(0.0);
                if score > best_score + 2 || (score >= best_score.saturating_sub(3) && cur_size > best_size + 5.0) {
                    best_score = score;
                    best = Some(path);
                }
            }
        }
    }
    if let Some(ref p) = best {
        let name = p.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
        let pstr = p.to_string_lossy().to_lowercase();
        if p.is_file() && !is_junk_exe(&name, &pstr) {
            return Some(p.clone());
        }
    }
    // Fallback: nothing non-junk was found in bounds - take the largest
    // plausible exe (>= 0.8 MB, not redist/installer/tooling junk).
    let mut largest: Option<(f64, PathBuf)> = None;
    let mut stack = vec![(install_root.to_path_buf(), 0usize)];
    let mut visited = 0;
    while let Some((dir, depth)) = stack.pop() {
        if visited > MAX_EXPLORE * 3 { break; }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            visited += 1;
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() {
                if depth < MAX_DEPTH + 4 && !should_skip(&path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default()) {
                    stack.push((path, depth + 1));
                }
            } else if ft.is_file() && path.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false) {
                let name = path.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
                let pstr = path.to_string_lossy().to_lowercase();
                if is_junk_exe(&name, &pstr) {
                    continue;
                }
                let sz = exe_size_mb(&path);
                if sz < 0.8 { continue; }
                match largest {
                    None => largest = Some((sz, path)),
                    Some((bsz, _)) => if sz > bsz { largest = Some((sz, path)); }
                }
            }
        }
    }
    largest.map(|(_, p)| p)
}

/// Executables that are never the game's render binary: installers, updaters,
/// dependency/redist helpers, middleware installers, and build/editor tools.
fn is_junk_exe(name: &str, path_lower: &str) -> bool {
    const BAD_NAME: &[&str] = &[
        "launcher",
        "installer",
        "updater",
        "crash",
        "unins",
        "setup",
        "directx",
        "dxsetup",
        "vcredist",
        "redist",
        "systemsoftware",
        "converter",
        "checker",
        "7zr",
        "build",
        "editor",
        "benchmark",
        "report",
        "helper",
        "physx",
        "dotnet",
    ];
    const BAD_PATH: &[&str] = &[
        r"\redist",
        r"\commonredist",
        r"\directx",
        r"\physx",
        r"\support",
        r"\vcredist",
        r"\dotnet",
        r"\_commonredist",
    ];
    BAD_NAME.iter().any(|k| name.contains(k)) || BAD_PATH.iter().any(|k| path_lower.contains(k))
}

fn score_exe_relative(path: &Path, root: &Path, root_lower: &str) -> i32 {
    let name = path
        .file_name()
        .map(|f| f.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let p = path.to_string_lossy().to_lowercase();
    let mut s: i32 = 0;

    if name.contains("shipping") {
        s += 120;
    }
    if p.contains("win64") || name.contains("win64") {
        s += 60;
    }
    if p.contains(r"\bin\x64") || p.contains(r"\binaries\win64") {
        s += 55;
    }
    if p.contains(r"\binaries") {
        s += 35;
    }
    if p.contains("_windows") {
        s += 20;
    }
    let stem = path.file_stem().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
    let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let nroot = norm(root_lower);
    let nstem = norm(&stem);
    if !nroot.is_empty() && (nstem.contains(&nroot) || nroot.contains(&nstem)) {
        s += 40;
    }

    if p.contains(r"\redist") || p.contains(r"\commonredist") || p.contains(r"\engine\binaries") {
        s -= 900;
    }
    if is_junk_exe(&name, &p) {
        s -= 400;
    }

    // Size-based boost: prefer larger exes (likely render)
    if let Ok(meta) = std::fs::metadata(path) {
        let mb = (meta.len() as f64) / (1024.0 * 1024.0);
        if mb > 40.0 {
            s += 60;
        } else if mb > 20.0 {
            s += 40;
        } else if mb > 10.0 {
            s += 25;
        } else if mb > 5.0 {
            s += 10;
        } else if mb < 0.5 {
            s -= 50;
        }
    }

    let depth = if let Ok(rel) = path.strip_prefix(root) {
        rel.components().count() as i32
    } else {
        p.matches(['\\', '/']).count() as i32
    };
    // Very gentle depth penalty
    s - depth * 1
}

fn exe_size_mb(path: &Path) -> f64 {
    std::fs::metadata(path).ok().map(|m| m.len() as f64 / (1024.0*1024.0)).unwrap_or(0.0)
}

/// All library folders listed in `libraryfolders.vdf`.
pub fn library_paths(steam_root: &Path) -> Result<Vec<PathBuf>, String> {
    let vdf = steam_root.join("steamapps").join("libraryfolders.vdf");
    let text = std::fs::read_to_string(&vdf).map_err(|e| format!("read {}: {e}", vdf.display()))?;
    let mut libs = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(r#""path""#) {
            if let Some(v) = extract_vdf_value(rest) {
                let p = PathBuf::from(v);
                if p.is_dir() {
                    libs.push(p);
                }
            }
        }
    }
    Ok(libs)
}

/// Read `appid` and `installdir` out of an `appmanifest_<id>.acf` file.
///
/// The filename alone already carries the AppID, but the file body is the
/// authoritative source and pairs it with the install folder name we need to
/// match against `steamapps/common`.
fn parse_appmanifest(path: &Path) -> Option<(u32, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut appid = None;
    let mut installdir = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(r#""appid""#) {
            if appid.is_none() {
                appid = extract_vdf_value(rest).and_then(|v| v.parse::<u32>().ok());
            }
        } else if let Some(rest) = t.strip_prefix(r#""installdir""#) {
            if installdir.is_none() {
                installdir = extract_vdf_value(rest).filter(|v| !v.is_empty());
            }
        }
    }
    // AppID also appears in the filename; use it if the body was unreadable.
    if appid.is_none() {
        let stem = path.file_stem()?.to_string_lossy().to_string();
        appid = stem.strip_prefix("appmanifest_").and_then(|s| s.parse::<u32>().ok());
    }
    Some((appid?, installdir?))
}

/// Map install-folder name -> AppID for every appmanifest in one library.
fn library_appids(steamapps: &Path) -> std::collections::HashMap<String, u32> {
    let mut map = std::collections::HashMap::new();
    let Ok(entries) = std::fs::read_dir(steamapps) else {
        return map;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
            continue;
        }
        if let Some((appid, dir)) = parse_appmanifest(&entry.path()) {
            map.insert(dir.to_lowercase(), appid);
        }
    }
    map
}

/// Scan every Steam library for installed games (folders under `steamapps/common`).
pub fn scan_installed_games(steam_root: &Path) -> Result<Vec<InstalledGame>, String> {
    let mut libs = library_paths(steam_root)?;
    let mut seen = std::collections::HashSet::new();
    libs.retain(|p| seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone())));
    if libs.is_empty() {
        let default = steam_root.join("steamapps");
        if default.is_dir() {
            libs.push(steam_root.to_path_buf());
        }
    }

    let mut result = Vec::new();
    for lib in &libs {
        let common = lib.join("steamapps").join("common");
        if !common.is_dir() {
            continue;
        }
        let appids = library_appids(&lib.join("steamapps"));
        let entries = std::fs::read_dir(&common)
            .map_err(|e| format!("read {}: {e}", common.display()))?;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_dir {
                continue;
            }
            let folder_name = entry.file_name().to_string_lossy().to_string();
            let appid = appids.get(&folder_name.to_lowercase()).copied();
            result.push(InstalledGame {
                folder_name,
                install_dir: entry.path(),
                detected_exe: None,
                source: "Steam".to_string(),
                appid,
                anti_cheat: AcScan::default(),
            });
        }
    }

    result.sort_by(|a, b| a.folder_name.to_lowercase().cmp(&b.folder_name.to_lowercase()));
    result.dedup_by(|a, b| a.install_dir == b.install_dir);
    Ok(result)
}

/// Extract a double-quoted VDF value from the remainder of a line.
/// Handles `\\` and `\"` escapes.
fn extract_vdf_value(s: &str) -> Option<String> {
    let s = s.trim();
    if !s.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for c in s[1..].chars() {
        if escaped {
            out.push(c);
            escaped = false;
        } else {
            match c {
                '\\' => escaped = true,
                '"' => return Some(out),
                _ => out.push(c),
            }
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_matches_folder_name_ignoring_separators() {
        // 007 First Light ships as `Retail\007FirstLight.exe`: the folder name has
        // spaces, the exe does not. The name match must still fire.
        let root = Path::new(r"C:\Games\007 First Light");
        let exe = Path::new(r"C:\Games\007 First Light\Retail\007FirstLight.exe");
        let score = score_exe_relative(exe, root, "007 first light");
        assert!(score >= 30, "expected a strong name match, got {score}");
    }

    #[test]
    fn junk_exes_are_rejected() {
        assert!(is_junk_exe(
            "physx_8.04.25_systemsoftware.exe",
            r"c:\games\dark sector\physx\physx_8.04.25_systemsoftware.exe"
        ));
        assert!(is_junk_exe(
            "vcredist_x86.exe",
            r"c:\games\alice\redistributable\vcredist_x86.exe"
        ));
        assert!(is_junk_exe(
            "buildstaticplevels.exe",
            r"c:\games\ccd\bin\win32\buildstaticplevels.exe"
        ));
        assert!(is_junk_exe(
            "unins000.exe",
            r"c:\games\x\unins000.exe"
        ));
        assert!(!is_junk_exe("ds.exe", r"c:\games\dark sector\ds.exe"));
        assert!(!is_junk_exe(
            "dyinglightgame.exe",
            r"c:\games\dying light\dyinglightgame.exe"
        ));
    }

    #[test]
    fn has_any_exe_detects_nested_binary() {
        let base = std::env::temp_dir().join(format!("dlssg_any_exe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let nested = base.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(base.join("readme.txt"), b"x").unwrap();
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!has_any_exe(&empty));
        assert!(!has_any_exe(&base));
        std::fs::write(nested.join("game.exe"), b"x").unwrap();
        assert!(has_any_exe(&base));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `reg query` output varies: leading blank lines, a key line, then the value
    /// line with a `REG_SZ` / `REG_EXPAND_SZ` type and a path that may contain
    /// spaces. The parser must find the value and only accept it if it exists.
    #[test]
    fn parse_steam_path_handles_reg_output() {
        let dir = std::env::temp_dir();
        let line = format!("\r\n    SteamPath    REG_EXPAND_SZ    {}\r\n", dir.display());
        assert_eq!(parse_steam_path(&line), Some(dir.clone()));
        assert_eq!(parse_steam_path("garbage"), None);
        assert_eq!(parse_steam_path("    SteamPath    REG_SZ    \r\n"), None);
    }

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        // One directory per call: tests run in parallel, and two of them write an
        // `appmanifest_2074650.acf`. A shared directory made those races flaky (a
        // peer could overwrite the file mid-test). The file *name* must stay exact
        // because `parse_appmanifest` derives a fallback AppID from it.
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dlssg-patcher-tests-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Real `appmanifest_*.acf` shape: tab-indented keys, quoted values, and a
    /// nested object. The AppID is what ties an install folder to a catalog entry,
    /// so a parsing slip here silently degrades every match to a name lookup.
    #[test]
    fn parses_appid_and_installdir_from_acf() {
        let p = write_temp(
            "appmanifest_1091500.acf",
            "\"AppState\"\n{\n\t\"appid\"\t\t\"1091500\"\n\t\"name\"\t\t\"Cyberpunk 2077\"\n\t\"installdir\"\t\t\"Cyberpunk 2077\"\n\t\"StateFlags\"\t\t\"4\"\n}\n",
        );
        assert_eq!(parse_appmanifest(&p), Some((1091500, "Cyberpunk 2077".to_string())));
    }

    /// Folder names with spaces and punctuation must survive intact.
    #[test]
    fn parses_installdir_with_punctuation() {
        let p = write_temp(
            "appmanifest_292030.acf",
            "\"AppState\"\n{\n\t\"appid\"\t\"292030\"\n\t\"installdir\"\t\"The Witcher 3\"\n}\n",
        );
        assert_eq!(parse_appmanifest(&p), Some((292030, "The Witcher 3".to_string())));
    }

/// Without an `installdir` the manifest cannot be tied to a folder, so it is
/// unusable for our map even though the filename still names an AppID.
    #[test]
    fn rejects_manifest_without_installdir() {
        let p = write_temp("appmanifest_2074650.acf", "");
        assert_eq!(parse_appmanifest(&p), None);
    }

    /// A readable folder name plus an AppID only present in the filename.
    #[test]
    fn falls_back_to_appid_in_filename() {
        let p = write_temp(
            "appmanifest_2074650.acf",
            "\"AppState\"\n{\n\t\"installdir\"\t\"Half-Life 2\"\n}\n",
        );
        assert_eq!(parse_appmanifest(&p), Some((2074650, "Half-Life 2".to_string())));
    }

    /// An ACF with no appid at all must not fabricate one.
    #[test]
    fn rejects_manifest_without_appid() {
        let p = write_temp("appmanifest_bogus.acf", "\"AppState\"\n{\n\t\"installdir\"\t\"X\"\n}\n");
        assert_eq!(parse_appmanifest(&p), None);
    }

    /// Non-ACF files in steamapps must be ignored, not misread.
    #[test]
    fn library_appids_ignores_other_files() {
        let dir = std::env::temp_dir().join("dlssg-patcher-tests-libs");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("libraryfolders.vdf"), "\"libraryfolders\"\n{\n}\n").unwrap();
        std::fs::write(
            dir.join("appmanifest_1091500.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\"1091500\"\n\t\"installdir\"\t\"Cyberpunk 2077\"\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("common"), "").unwrap();
        let map = library_appids(&dir);
        assert_eq!(map.get("cyberpunk 2077"), Some(&1091500));
        assert_eq!(map.len(), 1, "only real appmanifests should be indexed");
    }
}
