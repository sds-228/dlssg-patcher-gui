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
        assert!(err.contains("downloaded update") || err.contains("staged"), "{err}");
    }
}
