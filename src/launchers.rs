use std::path::PathBuf;
use std::os::windows::process::CommandExt;

use crate::anticheat::AcScan;
use crate::steam::{self, InstalledGame};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Discover installed games across every supported storefront/launcher.
///
/// Returns the combined, de-duplicated list plus a short per-launcher note
/// (e.g. `["Steam: 24", "Epic: 3", "GOG: 0", …]`) for the activity log. Missing
/// launchers are notes, never hard errors - a machine with only Steam still works.
pub fn scan_all(progress: &mut dyn FnMut(usize, usize)) -> (Vec<InstalledGame>, Vec<String>) {
    let mut games = Vec::new();
    let mut notes = Vec::new();
    let total = 5usize;
    let mut done = 0usize;

    match steam::steam_path_from_registry() {
        Some(root) => match steam::scan_installed_games(&root) {
            Ok(mut found) => {
                notes.push(format!("Steam: {}", found.len()));
                games.append(&mut found);
            }
            Err(e) => notes.push(format!("Steam: {e}")),
        },
        None => notes.push("Steam: not found".to_string()),
    }
    done += 1;
    progress(done, total);

    for (label, mut found) in [
        ("Epic", scan_epic()),
        ("GOG", scan_gog()),
        ("Ubisoft", scan_ubisoft()),
        ("EA", scan_ea()),
    ] {
        notes.push(format!("{label}: {}", found.len()));
        games.append(&mut found);
        done += 1;
        progress(done, total);
    }

    dedup(&mut games);
    (games, notes)
}

fn game(source: &str, name: String, dir: PathBuf) -> InstalledGame {
    let folder_name = if name.trim().is_empty() {
        dir.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        name
    };
    InstalledGame {
        folder_name,
        install_dir: dir,
        detected_exe: None,
        source: source.to_string(),
        // Epic and the other launchers we scan have no Steam AppID, so these
        // installs always match on folder or display name.
        appid: None,
        anti_cheat: AcScan::default(),
    }
}

/// Drop games that resolve to the same install folder (a game owned on two
/// launchers, or a store scanning the same directory twice).
fn dedup(games: &mut Vec<InstalledGame>) {
    let mut seen = std::collections::HashSet::new();
    games.retain(|g| {
        let key = g
            .install_dir
            .canonicalize()
            .unwrap_or_else(|_| g.install_dir.clone())
            .to_string_lossy()
            .to_lowercase();
        seen.insert(key)
    });
}

// ------------------------------------------------------------------ registry

/// Run `reg query <key> /s` and group the output by subkey. Each entry is
/// `(subkey_path, [(value_name, value_data), …])`. Returns empty on any failure
/// (missing key, no `reg.exe`) - callers treat that as "launcher not installed".
fn reg_query_values(key: &str) -> Vec<(String, Vec<(String, String)>)> {
    let out = match std::process::Command::new(crate::winutil::system32("reg.exe"))
        .args(["query", key, "/s"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);

    let mut result = Vec::new();
    let mut cur_key = String::new();
    let mut cur_vals: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("HKEY_") {
            if !cur_key.is_empty() {
                result.push((std::mem::take(&mut cur_key), std::mem::take(&mut cur_vals)));
            }
            cur_key = trimmed.trim().to_string();
        } else if let Some((name, _ty, value)) = parse_reg_line(trimmed) {
            cur_vals.push((name, value));
        }
    }
    if !cur_key.is_empty() {
        result.push((cur_key, cur_vals));
    }
    result
}

/// Parse one value line from `reg query` output. Value *names* can contain
/// spaces (EA uses `Install Dir`), so we locate the `REG_*` type token and split
/// around it instead of splitting on the first whitespace.
fn parse_reg_line(line: &str) -> Option<(String, String, String)> {
    const TYPES: [&str; 6] = [
        "REG_SZ",
        "REG_EXPAND_SZ",
        "REG_MULTI_SZ",
        "REG_DWORD",
        "REG_QWORD",
        "REG_BINARY",
    ];
    let t = line.trim();
    let (pos, ty) = TYPES
        .iter()
        .find_map(|ty| t.find(ty).map(|pos| (pos, *ty)))?;
    let name = t[..pos].trim().to_string();
    if name.is_empty() {
        return None;
    }
    let value = t[pos + ty.len()..].trim().to_string();
    Some((name, ty.to_string(), value))
}

fn reg_value(vals: &[(String, String)], name: &str) -> Option<String> {
    vals.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

// ---------------------------------------------------------------------- Epic

fn scan_epic() -> Vec<InstalledGame> {
    let mut games = Vec::new();
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    let manifests = program_data
        .join("Epic")
        .join("EpicGamesLauncher")
        .join("Data")
        .join("Manifests");
    let Ok(entries) = std::fs::read_dir(&manifests) else {
        return games;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().map(|x| x.eq_ignore_ascii_case("item")).unwrap_or(false) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let Some(loc) = v.get("InstallLocation").and_then(|x| x.as_str()) else { continue };
        if loc.trim().is_empty() {
            continue;
        }
        let dir = PathBuf::from(loc);
        if !dir.is_dir() {
            continue;
        }
        let name = v.get("DisplayName").and_then(|x| x.as_str()).unwrap_or("").to_string();
        games.push(game("Epic", name, dir));
    }
    games
}

// ----------------------------------------------------------------------- GOG

fn scan_gog() -> Vec<InstalledGame> {
    let mut games = Vec::new();
    for root in [
        r"HKLM\SOFTWARE\WOW6432Node\GOG.com\Games",
        r"HKLM\SOFTWARE\GOG.com\Games",
    ] {
        for (_key, vals) in reg_query_values(root) {
            let Some(path) = reg_value(&vals, "path") else { continue };
            if path.trim().is_empty() {
                continue;
            }
            let dir = PathBuf::from(&path);
            if dir.is_dir() {
                let name = reg_value(&vals, "gameName").unwrap_or_default();
                games.push(game("GOG", name, dir));
            }
        }
    }
    games
}

// ------------------------------------------------------------------ Ubisoft

fn scan_ubisoft() -> Vec<InstalledGame> {
    let mut games = Vec::new();
    for root in [
        r"HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs",
        r"HKLM\SOFTWARE\Ubisoft\Launcher\Installs",
    ] {
        for (key, vals) in reg_query_values(root) {
            let Some(dir_s) = reg_value(&vals, "InstallDir") else { continue };
            if dir_s.trim().is_empty() {
                continue;
            }
            let dir = PathBuf::from(&dir_s);
            if dir.is_dir() {
                let name = key.rsplit('\\').next().unwrap_or("").to_string();
                games.push(game("Ubisoft", name, dir));
            }
        }
    }
    games
}

// -------------------------------------------------------------- EA / Origin

fn scan_ea() -> Vec<InstalledGame> {
    let mut games = Vec::new();
    for root in [
        r"HKLM\SOFTWARE\WOW6432Node\Electronic Arts\EA Games",
        r"HKLM\SOFTWARE\Electronic Arts\EA Games",
        r"HKLM\SOFTWARE\WOW6432Node\Origin Games",
        r"HKLM\SOFTWARE\Origin Games",
    ] {
        for (key, vals) in reg_query_values(root) {
            let Some(dir_s) = reg_value(&vals, "Install Dir").or_else(|| reg_value(&vals, "InstallDir"))
            else {
                continue;
            };
            if dir_s.trim().is_empty() {
                continue;
            }
            let dir = PathBuf::from(&dir_s);
            if dir.is_dir() {
                let name = reg_value(&vals, "DisplayName")
                    .unwrap_or_else(|| key.rsplit('\\').next().unwrap_or("").to_string());
                games.push(game("EA", name, dir));
            }
        }
    }
    games
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_value_name_with_spaces() {
        let line = "    Install Dir    REG_SZ    C:\\Program Files\\EA Games\\Foo";
        let (name, ty, value) = parse_reg_line(line).unwrap();
        assert_eq!(name, "Install Dir");
        assert_eq!(ty, "REG_SZ");
        assert_eq!(value, r"C:\Program Files\EA Games\Foo");
    }

    #[test]
    fn parses_plain_value() {
        let line = "    path    REG_SZ    D:\\Games\\Cyberpunk 2077";
        let (name, _, value) = parse_reg_line(line).unwrap();
        assert_eq!(name, "path");
        assert_eq!(value, r"D:\Games\Cyberpunk 2077");
    }

    #[test]
    fn ignores_key_header_lines() {
        assert!(parse_reg_line(r"HKEY_LOCAL_MACHINE\SOFTWARE\Foo").is_none());
    }
}
