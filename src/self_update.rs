use std::path::{Path, PathBuf};
use std::time::Duration;

const RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/sds-228/dlssg-patcher-gui/releases/latest";
const USER_AGENT: &str = concat!("dlssg-patcher/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatcherRelease {
    pub version: String,
    pub tag: String,
    pub exe_url: String,
    pub asset_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatcherUpdateStatus {
    UpToDate { current: String },
    Available(PatcherRelease),
}

pub fn normalize_version_tag(tag: &str) -> String {
    tag.trim().trim_start_matches(['v', 'V']).to_string()
}

pub fn select_exe_asset_url(assets: &[serde_json::Value]) -> Option<(String, String)> {
    // Prefer the exact release asset name produced by this repo's workflow.
    for a in assets {
        let Some(name) = a.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if name.eq_ignore_ascii_case("dlssg-patcher.exe") {
            let url = a.get("browser_download_url").and_then(|v| v.as_str())?;
            return Some((url.to_string(), name.to_string()));
        }
    }
    None
}

pub fn parse_release_status(json: &[u8], current_version: &str) -> Result<PatcherUpdateStatus, String> {
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| format!("parse release json: {e}"))?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let version = normalize_version_tag(&tag);
    if version.is_empty() {
        return Err("latest release has no version tag".to_string());
    }
    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let (exe_url, asset_name) = select_exe_asset_url(&assets)
        .ok_or_else(|| "latest release has no dlssg-patcher.exe asset".to_string())?;

    let current = normalize_version_tag(current_version);
    let current_norm = if current.is_empty() {
        current_version.trim().to_string()
    } else {
        current.clone()
    };

    if crate::download::version_cmp(&version, &current_norm) == std::cmp::Ordering::Greater {
        Ok(PatcherUpdateStatus::Available(PatcherRelease {
            version,
            tag,
            exe_url,
            asset_name,
        }))
    } else {
        Ok(PatcherUpdateStatus::UpToDate { current: current_norm })
    }
}

fn fetch_json(url: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .timeout_connect(Some(Duration::from_secs(6)))
        .build()
        .new_agent();
    let mut resp = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("update check failed: {e}"))?;
    resp.body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("read release response: {e}"))
}

pub fn check_patcher_update() -> Result<PatcherUpdateStatus, String> {
    let json = fetch_json(RELEASES_LATEST_URL)?;
    parse_release_status(&json, env!("CARGO_PKG_VERSION"))
}

pub fn staged_update_path(cache_root: &Path, version: &str) -> PathBuf {
    cache_root
        .join("updates")
        .join(format!("dlssg-patcher-{}.exe", normalize_version_tag(version)))
}

pub fn download_patcher_update(
    release: &PatcherRelease,
    dest: &Path,
    progress: Option<Box<dyn Fn(String) + Send>>,
) -> Result<PathBuf, String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    if let Some(cb) = &progress {
        cb(format!("Downloading patcher update {}…", release.version));
    }

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .build()
        .new_agent();
    let mut resp = agent
        .get(&release.exe_url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;

    let tmp = dest.with_extension("download");
    let mut reader = resp.body_mut().with_config().limit(512 * 1024 * 1024).reader();
    let mut file = std::fs::File::create(&tmp)
        .map_err(|e| format!("create {}: {e}", tmp.display()))?;
    let n = std::io::copy(&mut reader, &mut file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("write update: {e}")
    })?;
    drop(file);
    if n == 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err("downloaded update is empty".to_string());
    }

    // Best-effort replace on Windows: remove existing dest then rename.
    if dest.exists() {
        let _ = std::fs::remove_file(dest);
    }
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("finalize update download: {e}")
    })?;

    if let Some(cb) = &progress {
        cb(format!("Downloaded patcher update {}.", release.version));
    }
    Ok(dest.to_path_buf())
}

pub fn backup_exe_path(current_exe: &Path) -> PathBuf {
    let mut name = current_exe
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "dlssg-patcher.exe".into());
    name.push(".old");
    current_exe.with_file_name(name)
}

/// Pure-ish preflight for apply decisions. `can_replace` indicates whether the
/// current install location appears writable/replaceable.
pub fn preflight_apply(
    _current_exe: &Path,
    staged_exe: &Path,
    can_replace: bool,
) -> Result<(), String> {
    if !staged_exe.is_file() {
        return Err(format!(
            "Downloaded update not found at {}. Please download the update again.",
            staged_exe.display()
        ));
    }
    if !can_replace {
        return Err(format!(
            "Cannot replace the installed app in this location. The downloaded update is saved at {} — replace the app manually.",
            staged_exe.display()
        ));
    }
    Ok(())
}

fn can_replace_in_place(current_exe: &Path) -> bool {
    let Some(parent) = current_exe.parent() else {
        return false;
    };
    let probe = parent.join(format!(".dlssg-patcher-update-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Best-effort cleanup of leftovers from a previous self-update attempt.
/// Safe to call on startup; errors are ignored.
pub fn cleanup_update_leftovers() {
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    // Remove old renamed exe next to the running binary, if present.
    let old = backup_exe_path(&current);
    let _ = std::fs::remove_file(old);

    // Remove stale temp download files in a sibling/common updates folder if the
    // running exe lives there (best-effort, non-recursive).
    if let Some(parent) = current.parent() {
        let updates = parent.join("updates");
        if updates.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&updates) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("download"))
                    {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
    }
}

/// Apply a previously downloaded patcher update by swapping the current exe and
/// relaunching. On success this spawns the replacement process and returns `Ok(())`;
/// the caller should exit the current app. On failure it returns an actionable
/// error and leaves the current installation usable (manual fallback info included).
pub fn apply_update_on_restart(staged_exe: &Path) -> Result<(), String> {
    let current = std::env::current_exe()
        .map_err(|e| format!("cannot determine current app path: {e}"))?;
    let can_replace = can_replace_in_place(&current);
    preflight_apply(&current, staged_exe, can_replace)?;

    let backup = backup_exe_path(&current);
    let _ = std::fs::remove_file(&backup);

    // Rename running exe aside so we can place the new one at the original path.
    std::fs::rename(&current, &backup).map_err(|e| {
        format!(
            "Cannot replace the installed app in this location ({}). The downloaded update is saved at {} — replace the app manually.",
            e,
            staged_exe.display()
        )
    })?;

    // Place staged update at the current exe path.
    let place_result = std::fs::rename(staged_exe, &current).or_else(|_| {
        std::fs::copy(staged_exe, &current).map(|_| {
            let _ = std::fs::remove_file(staged_exe);
        })
    });
    if let Err(e) = place_result {
        // Try to restore the previous exe.
        let _ = std::fs::rename(&backup, &current);
        return Err(format!(
            "Failed to install the downloaded update ({}). The downloaded update is saved at {} — replace the app manually.",
            e,
            staged_exe.display()
        ));
    }

    // Relaunch the new exe. If spawn fails, keep both files and report fallback.
    if let Err(e) = std::process::Command::new(&current).spawn() {
        return Err(format!(
            "Update installed but failed to restart automatically ({}). Please start the app again from {}.",
            e,
            current.display()
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_tag_strips_leading_v() {
        assert_eq!(normalize_version_tag("v2.0.3"), "2.0.3");
        assert_eq!(normalize_version_tag("V2.0.3"), "2.0.3");
        assert_eq!(normalize_version_tag("2.0.3"), "2.0.3");
    }

    #[test]
    fn select_exe_asset_prefers_exact_patcher_exe() {
        let assets = vec![
            serde_json::json!({"name":"dlssg-patcher-windows.zip","browser_download_url":"https://example/zip"}),
            serde_json::json!({"name":"dlssg-patcher.exe","browser_download_url":"https://example/exe"}),
        ];
        let (url, name) = select_exe_asset_url(&assets).expect("asset");
        assert_eq!(name, "dlssg-patcher.exe");
        assert_eq!(url, "https://example/exe");
    }

    #[test]
    fn parse_release_json_reports_available_when_newer() {
        let json = br#"{
          "tag_name":"v9.9.9",
          "assets":[{"name":"dlssg-patcher.exe","browser_download_url":"https://example/dlssg-patcher.exe"}]
        }"#;
        match parse_release_status(json, "2.0.2").unwrap() {
            PatcherUpdateStatus::Available(r) => {
                assert_eq!(r.version, "9.9.9");
                assert_eq!(r.tag, "v9.9.9");
                assert_eq!(r.asset_name, "dlssg-patcher.exe");
                assert_eq!(r.exe_url, "https://example/dlssg-patcher.exe");
            }
            other => panic!("expected available, got {other:?}"),
        }
    }

    #[test]
    fn parse_release_json_reports_up_to_date_when_not_newer() {
        let json = br#"{
          "tag_name":"v2.0.2",
          "assets":[{"name":"dlssg-patcher.exe","browser_download_url":"https://example/dlssg-patcher.exe"}]
        }"#;
        match parse_release_status(json, "2.0.2").unwrap() {
            PatcherUpdateStatus::UpToDate { current } => assert_eq!(current, "2.0.2"),
            other => panic!("expected up-to-date, got {other:?}"),
        }
    }

    #[test]
    fn staged_update_path_uses_updates_folder_and_normalized_version() {
        let p = staged_update_path(Path::new("C:\\cache"), "v2.0.3");
        assert_eq!(
            p,
            Path::new("C:\\cache")
                .join("updates")
                .join("dlssg-patcher-2.0.3.exe")
        );
    }

    #[test]
    fn backup_path_for_current_exe_uses_old_suffix() {
        let p = backup_exe_path(Path::new("C:\\apps\\dlssg-patcher.exe"));
        assert_eq!(p, Path::new("C:\\apps\\dlssg-patcher.exe.old"));
    }

    #[test]
    fn apply_requires_staged_exe_to_exist() {
        let err = preflight_apply(
            Path::new("C:\\apps\\dlssg-patcher.exe"),
            Path::new("C:\\apps\\missing-update.exe"),
            false,
        )
        .unwrap_err();
        let e = err.to_lowercase();
        assert!(e.contains("download") || e.contains("staged") || e.contains("not found"), "{err}");
    }
}
