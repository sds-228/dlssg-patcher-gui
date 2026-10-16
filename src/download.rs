use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

/// Upstream repo (`owner/name`) that ships the prebuilt proxy + INI files.
const REPO: &str = "sdli1995/dlssg_for_sm86";
/// `main` branch root, used by `resolve_tag` as a fallback when the GitHub API is
/// unreachable.
const RAW_BASE: &str = "https://raw.githubusercontent.com/sdli1995/dlssg_for_sm86/main/";

/// SHA-256 of known upstream release zips, keyed by version without the leading
/// `v`. A known version's zip is verified before extraction; an unknown (newer)
/// version is still accepted but reported as unverified.
const PINNED_RELEASE_SHA256: &[(&str, &str)] = &[(
    "0.4.0",
    "5865360c3295038f0ff979d303bb7eb9437eec09fbc08686967d3b5d41d9c019",
)];

/// Lowercase hex SHA-256 of a byte slice.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The pinned hash for a version, accepting an optional leading `v`.
fn pinned_release_hash(version: Option<&str>) -> Option<&'static str> {
    let v = version?.trim().trim_start_matches('v');
    PINNED_RELEASE_SHA256
        .iter()
        .find(|(k, _)| *k == v)
        .map(|(_, h)| *h)
}

/// Outcome of checking a fetched release zip against the pinned hashes.
#[derive(Debug, PartialEq, Eq)]
enum ZipTrust {
    /// A pinned hash exists and matches the bytes.
    Trusted,
    /// No pinned hash for this version; accepted, but not verifiable.
    Unverified,
    /// A pinned hash exists and does not match: refuse the bytes.
    Mismatch,
}

/// Classify a fetched release zip for `version` against `table`.
fn verify_release_zip(table: &[(&str, &str)], version: Option<&str>, bytes: &[u8]) -> ZipTrust {
    let v = version.map(|s| s.trim().trim_start_matches('v'));
    match v.and_then(|v| table.iter().find(|(k, _)| *k == v).map(|(_, h)| *h)) {
        Some(expected) if sha256_hex(bytes) == expected => ZipTrust::Trusted,
        Some(_) => ZipTrust::Mismatch,
        None => ZipTrust::Unverified,
    }
}

/// Mirror base URLs to try, in order. Tag bases (immutable) come first, then the
/// branch as a fallback so a tag/API outage never blocks patching.
fn mirror_bases(tag: Option<&str>) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(t) = tag {
        v.push(format!("https://raw.githubusercontent.com/{REPO}/{t}/"));
        v.push(format!("https://cdn.jsdelivr.net/gh/{REPO}@{t}/"));
    }
    v.push(format!("https://raw.githubusercontent.com/{REPO}/main/"));
    v.push(format!("https://cdn.jsdelivr.net/gh/{REPO}@main/"));
    v
}

/// Accept only a plain dotted version (optionally `v`-prefixed) as a URL path
/// segment, so a hostile or odd `tag_name` can never inject a path or redirect
/// the fetch. `None` means "fall back to the branch".
fn sanitize_tag(t: &str) -> Option<&str> {
    let t = t.trim();
    let core = t.strip_prefix('v').unwrap_or(t);
    let mut parts = core.split('.');
    let a = parts.next()?;
    let b = parts.next()?;
    let c = parts.next();
    if parts.next().is_some() {
        return None; // more than three components
    }
    if a.is_empty()
        || b.is_empty()
        || !a.chars().all(|ch| ch.is_ascii_digit())
        || !b.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    if let Some(c) = c {
        if c.is_empty() || !c.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
    }
    Some(t)
}

/// Latest release tag from the GitHub API; fall back to the version advertised on
/// `main` (README) when the API is unreachable or rate-limited.
fn resolve_tag() -> Option<String> {
    if let Ok(json) = fetch_bytes_timeout(
        "https://api.github.com/repos/sdli1995/dlssg_for_sm86/releases/latest",
        8,
        4,
    ) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&json) {
            if let Some(t) = v.get("tag_name").and_then(|x| x.as_str()) {
                if sanitize_tag(t).is_some() {
                    return Some(t.to_string());
                }
            }
        }
    }
    fetch_once_from_sources(&[RAW_BASE.to_string()], "README.en.md", "readme", &None)
        .ok()
        .and_then(|b| parse_version_from_readme(&String::from_utf8_lossy(&b)))
        .filter(|v| sanitize_tag(v).is_some())
}

/// Where the 0.4.0 bundle comes from: the release zip, plus the tag it was published under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseRef {
    pub tag: Option<String>,
    pub zip_url: Option<String>,
}

/// Ask the GitHub API for the latest release and the download URL of its `.zip` asset.
/// Best-effort: `None` fields mean "try the constructed URLs instead".
fn resolve_release() -> ReleaseRef {
    let mut out = ReleaseRef::default();
    if let Ok(json) = fetch_bytes_timeout(
        "https://api.github.com/repos/sdli1995/dlssg_for_sm86/releases/latest",
        8,
        4,
    ) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&json) {
            if let Some(t) = v.get("tag_name").and_then(|x| x.as_str()) {
                if sanitize_tag(t).is_some() {
                    out.tag = Some(t.to_string());
                }
            }
            out.zip_url = v
                .get("assets")
                .and_then(|a| a.as_array())
                .and_then(|assets| {
                    assets.iter().find_map(|a| {
                        let name = a.get("name")?.as_str()?;
                        let url = a.get("browser_download_url")?.as_str()?;
                        if name.ends_with(".zip") && url.starts_with("https://github.com/") {
                            Some(url.to_string())
                        } else {
                            None
                        }
                    })
                });
        }
    }
    out
}

/// Candidate zip URLs, in order, for a release with this tag/version.
/// Exposed for tests; `tag` and `ver` are the same value with and without the `v` prefix.
fn zip_urls(tag: Option<&str>, ver: Option<&str>) -> Vec<String> {
    let mut v = Vec::new();
    let ver = ver.or(tag.map(|t| t.trim_start_matches('v')))
        .filter(|v| sanitize_tag(v).is_some());
    let Some(ver) = ver else { return v };
    let name = format!("dlssg-release-{ver}-x64.zip");
    for tag in [tag.map(|t| t.to_string()), Some(ver.to_string()), Some(format!("v{ver}"))].into_iter().flatten() {
        let u = format!("https://github.com/{REPO}/releases/download/{tag}/{name}");
        if !v.contains(&u) {
            v.push(u);
        }
    }
    v
}

/// Proxy DLL entry points shipped by upstream v0.4.0, in upstream's own priority order.
/// Only ONE may sit beside a render exe at a time: when a game refuses `version.dll`,
/// the user swaps to the next name — upstream's rule is "swap, do not add".
pub const PROXY_NAMES: &[&str] = &[
    "version.dll",
    "winmm.dll",
    "dbghelp.dll",
    "dinput8.dll",
    "dxgi.dll",
    "d3d12.dll",
];

pub const INI_NAME: &str = "dlssg_sm86.ini";

/// The runtime tree every 0.4.0 patch needs beside the render exe. Upstream: "copying only
/// the DLL does nothing" — the loader in the proxy reads these.
pub const RUNTIME_DIR_NAME: &str = "dlssg_sm86";

/// The 4 payload files inside `RUNTIME_DIR_NAME` that a usable patch must have, as paths
/// relative to it. Verified against the 0.4.0 release zip (`sr/` is optional and excluded).
pub const RUNTIME_REQUIRED: &[&str] = &[
    "runtime/nvngx_dlssg.dll",
    "backend/sm86_backend.dll",
    "kernels/fg_kernels.dll",
    "xefg/xefg_kernels.dll",
];

/// Super-resolution payload: release-zip only (~180 MB) and `Mode=Off` by default.
pub const RUNTIME_SR_DIR: &str = "sr";

/// Cache sidecar that records which dlssg_for_sm86 version we are on. The
/// upstream INI no longer carries a "; Native x.y.z" header, so the version is
/// persisted here after a download and reused on later (offline) launches.
const VERSION_FILE: &str = "version.txt";

fn read_cached_version(cache: &Path) -> Option<String> {
    std::fs::read_to_string(cache.join(VERSION_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_cached_version(cache: &Path, v: &str) {
    let v = v.trim();
    let _ = std::fs::write(cache.join(VERSION_FILE), v);
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum FileSource {
    Github,
    Cached,
    Local,
    Missing(Vec<String>),
}

impl FileSource {
    pub fn label(&self) -> String {
        match self {
            FileSource::Github => "GitHub (fresh download)".to_string(),
            FileSource::Cached => "downloaded + cached".to_string(),
            FileSource::Local => "local files/ folder".to_string(),
            FileSource::Missing(files) => {
                if files.is_empty() {
                    "unknown".to_string()
                } else {
                    format!("missing: {}", files.join(", "))
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModFiles {
    pub source: FileSource,
    /// Version read from the release tag, README, or version.txt sidecar (e.g. "0.4.0").
    pub version: Option<String>,
    /// Cache dir containing `proxy/<name>.dll`, `dlssg_sm86.ini`, and `dlssg_sm86/`.
    pub cache_dir: PathBuf,
}

/// Path of a proxy DLL inside the repo. Upstream renamed `altnative/` to `alternatives/`
/// in 0.4.0; `version.dll` is the only one at the root.
fn proxy_rel(name: &str) -> String {
    if name == "version.dll" {
        "version.dll".to_string()
    } else {
        format!("alternatives/{name}")
    }
}

fn ini_rel() -> &'static str {
    INI_NAME
}

/// Number of full mirror passes before giving up on a file (GitHub fails often on
/// some networks, so each mirror is tried several times before erroring out).
const ATTEMPTS: u32 = 2;

/// Try downloading `<rel>` from each mirror in order, several passes.
/// Returns the first success.
fn fetch_from_sources(
    bases: &[String],
    rel: &str,
    label: &str,
    progress: &Option<Box<dyn Fn(String) + Send>>,
) -> Result<Vec<u8>, String> {
    let mut last_err = String::new();
    for attempt in 0..ATTEMPTS {
        for base in bases {
            let url = format!("{base}{rel}");
            if attempt > 0 {
                if let Some(cb) = progress {
                    cb(format!("Retrying {label} (attempt {})…", attempt + 1));
                }
            } else if let Some(cb) = progress {
                cb(format!("Fetching {label} …"));
            }
            match fetch_bytes(&url) {
                Ok(bytes) => {
                    if let Some(cb) = progress {
                        cb(format!("Fetched {label} ({:.1} MB)", bytes.len() as f64 / (1024.0 * 1024.0)));
                    }
                    return Ok(bytes);
                }
                Err(e) => last_err = format!("{url}: {e}"),
            }
        }
    }
    Err(last_err)
}

fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    fetch_bytes_timeout(url, 30, 8)
}

/// Upper bound on a single response body, sized to hold the release payload with headroom.
const BODY_LIMIT: usize = 256 * 1024 * 1024;

fn fetch_bytes_timeout(url: &str, global_secs: u64, connect_secs: u64) -> Result<Vec<u8>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(global_secs)))
        .timeout_connect(Some(Duration::from_secs(connect_secs)))
        .build()
        .new_agent();
    let mut resp = agent
        .get(url)
        .header("User-Agent", "dlssg-patcher/0.1")
        .call()
        .map_err(|e| format!("HTTP error: {e}"))?;
    // Raise read_to_vec()'s default limit; the release payload is far larger.
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT as u64)
        .read_to_vec()
        .map_err(|e| format!("read body: {e}"))?;
    Ok(bytes)
}

/// Per-read stall window, and the hard cap for one attempt at the zip.
const ZIP_STALL_SECS: u64 = 30;
const ZIP_CAP_SECS: u64 = 900;
const ZIP_PROGRESS_STEP: u64 = 8 * 1024 * 1024;

/// Drain a chunk channel with a stall detector. Pure — no network, no clock but `Instant`.
fn drain_with_stall(
    rx: std::sync::mpsc::Receiver<Result<(u64, Vec<u8>), String>>,
    stall: Duration,
    cap: Duration,
    step: u64,
    progress: &Option<Box<dyn Fn(String) + Send>>,
) -> Result<Vec<u8>, String> {
    use std::sync::mpsc::RecvTimeoutError;
    let started = std::time::Instant::now();
    let mut out: Vec<u8> = Vec::new();
    let mut announced = 0u64;
    loop {
        let left = cap.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(format!(
                "download exceeded the {}s cap after {} bytes",
                cap.as_secs(),
                out.len()
            ));
        }
        match rx.recv_timeout(stall.min(left)) {
            Ok(Ok((total, chunk))) => {
                out.extend_from_slice(&chunk);
                if let Some(cb) = progress {
                    if total.saturating_sub(announced) >= step {
                        announced = total;
                        cb(zip_progress_line(out.len(), started.elapsed()));
                    }
                }
            }
            // The worker's own error is the real reason the transfer stopped; never swallow it.
            Ok(Err(e)) => return Err(e),
            // Nothing arrived inside the window: the line is dead even though it has not
            // formally ended. Nothing has been written to disk, so giving up is safe. When the
            // slice we waited on was the remainder of the cap rather than the window, the cap
            // is what ended it.
            Err(RecvTimeoutError::Timeout) => {
                if started.elapsed() >= cap {
                    return Err(format!(
                        "download exceeded the {}s cap after {} bytes",
                        cap.as_secs(),
                        out.len()
                    ));
                }
                return Err(format!(
                    "download stalled: no data for {}s after {} bytes",
                    stall.as_secs(),
                    out.len()
                ));
            }
            // The worker dropped its sender: end of body.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if let Some(cb) = progress {
        cb(zip_progress_line(out.len(), started.elapsed()));
    }
    Ok(out)
}

/// The one line the release download prints, e.g. `Fetched release bundle … 8.0 MB (4.2 MB/s)`.
fn zip_progress_line(bytes: usize, elapsed: Duration) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    let secs = elapsed.as_secs_f64().max(0.001);
    format!("Fetched release bundle … {mb:.1} MB ({:.1} MB/s)", mb / secs)
}

/// Download a large asset with a stall detector instead of a flat clock.
fn fetch_zip_with_stall(
    url: &str,
    progress: &Option<Box<dyn Fn(String) + Send>>,
) -> Result<Vec<u8>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(8)))
        // The worker ends itself via its own total body budget, so a caller that walks away
        // cannot leave a socket being read for ever.
        .timeout_recv_body(Some(Duration::from_secs(ZIP_CAP_SECS)))
        .build()
        .new_agent();
    let resp = agent
        .get(url)
        .header("User-Agent", "dlssg-patcher/0.1")
        .call()
        .map_err(|e| format!("HTTP error: {e}"))?;
    // The headers are in hand before the body is touched, so the expected size can be named
    // while the reader itself moves to the worker thread.
    let body = resp.into_body();
    if let (Some(cb), Some(len)) = (progress, body.content_length()) {
        cb(format!("Release bundle is {:.1} MB", len as f64 / (1024.0 * 1024.0)));
    }
    let mut reader = body.into_with_config().limit(BODY_LIMIT as u64).reader();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut buf = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total += n as u64;
                    if tx.send(Ok((total, buf[..n].to_vec()))).is_err() {
                        break; // the caller gave up; there is nothing on disk to clean up
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("read body: {e}")));
                    break;
                }
            }
        }
    });
    drain_with_stall(
        rx,
        Duration::from_secs(ZIP_STALL_SECS),
        Duration::from_secs(ZIP_CAP_SECS),
        ZIP_PROGRESS_STEP,
        progress,
    )
}

/// Single pass over the mirrors (no retry loop) - for small metadata such as the
/// version INI/README where retrying every mirror just adds dead time.
fn fetch_once_from_sources(
    bases: &[String],
    rel: &str,
    label: &str,
    progress: &Option<Box<dyn Fn(String) + Send>>,
) -> Result<Vec<u8>, String> {
    let mut last_err = String::new();
    for base in bases {
        let url = format!("{base}{rel}");
        if let Some(cb) = progress {
            cb(format!("Fetching {label} …"));
        }
        match fetch_bytes(&url) {
            Ok(bytes) => return Ok(bytes),
            Err(e) => last_err = format!("{url}: {e}"),
        }
    }
    Err(last_err)
}

fn parse_version_from_ini(text: &str) -> Option<String> {
    // First line looks like: `; Native 0.2.4. Restart the game...`
    text.lines().find_map(|l| {
        let t = l.trim().trim_start_matches(';').trim();
        if let Some(rest) = t.strip_prefix("Native ") {
            rest.split_whitespace().next().map(|v| v.trim_end_matches('.').to_string())
        } else {
            None
        }
    })
}

/// Parse a dotted version from the upstream README title, e.g.
/// `# DLSSG for SM86 (proxy) - 0.3.0 Version`.
fn parse_version_from_readme(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let t = l.trim().trim_start_matches('#').trim();
        let rest = t.split(['-', '–', '—']).nth(1)?.trim();
        let mut head = rest.split_whitespace().next()?.to_string();
        head = head.trim_start_matches('v').to_string();
        if head.chars().any(|c| c.is_ascii_digit()) {
            Some(head)
        } else {
            None
        }
    })
}

/// Latest upstream state detected by the update check.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    /// Version string implied upstream (e.g. "0.3.0").
    pub remote_version: String,
    /// Whole hours since the repo was last pushed, if the GitHub API was reachable.
    pub pushed_hours_ago: Option<u64>,
}

/// Compare dotted versions numerically (`0.3.0` > `0.2.4`).
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    version_triple(a).cmp(&version_triple(b))
}

fn version_triple(v: &str) -> (u32, u32, u32) {
    let mut parts = v
        .split('.')
        .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Highest of the version strings we managed to read, if any.
fn newest_version(candidates: &[Option<String>]) -> Option<String> {
    candidates
        .iter()
        .flatten()
        .filter(|v| sanitize_tag(v).is_some())
        .max_by(|a, b| version_cmp(a, b))
        .cloned()
}

/// Check the push date of the upstream repo and the current version on `main`.
/// Never throws - reachability issues come back as a normal Error.
pub fn check_for_update() -> Result<UpdateInfo, String> {
    let pushed_hours_ago = repo_pushed_hours_ago();
    // Resolve the tag once so the reported version and the bytes it is read from
    // agree, and so the check talks to the same immutable snapshot as a patch.
    let tag = resolve_tag();
    let bases = mirror_bases(tag.as_deref());
    let ini_version = fetch_once_from_sources(&bases, ini_rel(), "ini", &None)
        .ok()
        .and_then(|b| parse_version_from_ini(&String::from_utf8_lossy(&b)));
    let readme_version = fetch_once_from_sources(&bases, "README.en.md", "readme", &None)
        .ok()
        .and_then(|b| parse_version_from_readme(&String::from_utf8_lossy(&b)));
    // The tag is the snapshot the release zip is published under, so it outranks nothing by
    // itself but is the one source that survives an INI and a README that both lack a version.
    let tag_version = tag.as_deref().map(|t| t.trim_start_matches('v').to_string());
    let remote_version = newest_version(&[ini_version, readme_version, tag_version])
        .ok_or_else(|| "could not read the upstream version number (INI, README and tag all lack one)".to_string())?;
    Ok(UpdateInfo { remote_version, pushed_hours_ago })
}

fn repo_pushed_hours_ago() -> Option<u64> {
    let json = fetch_bytes_timeout("https://api.github.com/repos/sdli1995/dlssg_for_sm86", 8, 4).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&json).ok()?;
    let pushed = v.get("pushed_at")?.as_str()?;
    let unix = rfc3339_unix(pushed)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.checked_sub(unix).map(|s| s / 3600)
}

/// Parse an RFC3339 UTC timestamp such as `2026-09-14T12:00:00Z` into unix seconds.
fn rfc3339_unix(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let time = rest.split(['Z', '+', '-']).next().unwrap_or("00:00:00");
    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next().map(|x| x.parse::<i64>().unwrap_or(0)).unwrap_or(0);
    let sec: i64 = tp
        .next()
        .map(|x| x.trim_end_matches('Z').parse::<i64>().unwrap_or(0))
        .unwrap_or(0);
    let days = days_from_civil(y, mo, d);
    Some((days * 86_400 + h * 3_600 + mi * 60 + sec) as u64)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// True when `dir` holds the payload files a patch needs (`RUNTIME_REQUIRED`).
/// "At least one DLL" would accept a half-extracted cache as complete.
pub fn is_runtime_dir_usable(dir: &Path) -> bool {
    dir.is_dir() && RUNTIME_REQUIRED.iter().all(|rel| is_usable(&dir.join(rel)))
}

/// True when the cache can patch a game without touching the network: all six proxies,
/// the INI, and a complete runtime tree. A 0.3.5-era cache fails this by construction,
/// which is what makes an upgrade fetch the 0.4.0 bundle on launch.
fn cache_is_complete(cache: &Path) -> bool {
    let proxy_dir = cache.join("proxy");
    PROXY_NAMES.iter().all(|n| is_usable(&proxy_dir.join(n)))
        && is_usable(&cache.join(INI_NAME))
        && is_runtime_dir_usable(&cache.join(RUNTIME_DIR_NAME))
}

/// The cached version of an existing bundle: the sidecar first, then the legacy INI header.
fn cached_mod_version(cache: &Path) -> Option<String> {
    read_cached_version(cache).or_else(|| {
        let ini_text = std::fs::read_to_string(cache.join(INI_NAME)).unwrap_or_default();
        parse_version_from_ini(&ini_text)
    })
}

/// A real `dlssg-release-*.zip` in one of the local `files/` folders, if the user keeps one.
fn find_local_zip(local_dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in local_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            if name.starts_with("dlssg-release-") && name.ends_with(".zip") && is_usable(&path) {
                return Some(path);
            }
        }
    }
    None
}

/// A local `dlssg_sm86/` tree in one of the `files/` folders. The repo mirrors cannot supply
/// it, so a local copy is the only offline source besides the release zip.
fn find_local_runtime_dir(local_dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in local_dirs {
        let candidate = dir.join(RUNTIME_DIR_NAME);
        if is_runtime_dir_usable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Recursively copy `src` into `dst`, creating directories as needed.
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", src.display()))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Ensure the complete 0.4.0 bundle is cached: all six proxy DLLs, the INI and the
/// `dlssg_sm86` runtime tree. Order is the release zip first (the only source of the runtime
/// tree), then the raw mirror files (GitHub → jsDelivr), then local `files/` folders.
///
/// `force` re-downloads everything; otherwise a cache that can already patch a game is reused
/// without any network call. `progress` (if any) is called with status lines while working.
pub fn ensure_mod_files(
    cache_root: &Path,
    local_dirs: &[PathBuf],
    force: bool,
    progress: Option<Box<dyn Fn(String) + Send>>,
) -> Result<ModFiles, String> {
    let cache = cache_root.join("cache");
    let proxy_dir = cache.join("proxy");
    std::fs::create_dir_all(&proxy_dir).map_err(|e| format!("create {}: {e}", proxy_dir.display()))?;

    // 1. A cache that can already patch a game needs no network at all.
    if !force && cache_is_complete(&cache) {
        if let Some(cb) = &progress {
            cb("Mod files already cached".to_string());
        }
        return Ok(ModFiles {
            source: FileSource::Cached,
            version: cached_mod_version(&cache),
            cache_dir: cache,
        });
    }

    // The release is resolved ONCE: its tag and its asset URL have to agree, and asking
    // `/releases/latest` twice (as `resolve_tag` would) can straddle a release.
    let rel = resolve_release();
    let bases = mirror_bases(rel.tag.as_deref());
    let cache_ini = cache.join(INI_NAME);

    let mut downloaded: HashMap<String, usize> = HashMap::new();
    let mut from_local = false;
    let mut fetched_from_zip = false;

    // Pick the newest valid version across the README and the resolved release tag
    // (matching `check_for_update`), so a stale README header on `main` never downgrades
    // the recorded version of a newer release tag or traps the UI in an update loop.
    let readme_text = fetch_once_from_sources(&bases, "README.en.md", "readme", &progress)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).to_string());
    let readme_version = readme_text.as_deref().and_then(parse_version_from_readme);
    let tag_version = rel.tag.as_deref().map(|t| t.trim_start_matches('v').to_string());
    let resolved_version = newest_version(&[readme_version, tag_version]);
    let version_from_readme_or_tag: Option<&str> = resolved_version.as_deref();

    // 2. The release zip first: it is the only source of the runtime tree.
    let mut zip_candidates: Vec<String> = rel.zip_url.iter().cloned().collect();
    zip_candidates.extend(zip_urls(rel.tag.as_deref(), version_from_readme_or_tag));
    for url in &zip_candidates {
        match fetch_zip_with_stall(url, &progress) {
            Ok(bytes) => {
                match verify_release_zip(PINNED_RELEASE_SHA256, version_from_readme_or_tag, &bytes) {
                    ZipTrust::Mismatch => {
                        let expected =
                            pinned_release_hash(version_from_readme_or_tag).unwrap_or("(unknown)");
                        if let Some(cb) = &progress {
                            cb(format!("Release zip failed its integrity check (expected sha256 {expected}); refusing it."));
                        }
                        eprintln!("[dlssg-patcher] release zip sha256 mismatch for {url}; refusing");
                        continue;
                    }
                    ZipTrust::Unverified => {
                        if let Some(cb) = &progress {
                            cb(format!(
                                "Release {} has no pinned hash; installing it as unverified.",
                                version_from_readme_or_tag.unwrap_or("(unknown)")
                            ));
                        }
                    }
                    ZipTrust::Trusted => {}
                }
                extract_release_zip(&bytes, &cache, false)?;
                // Stop at the first URL that leaves a complete, patchable cache.
                if cache_is_complete(&cache) {
                    fetched_from_zip = true;
                    break;
                }
            }
            Err(e) => eprintln!("[dlssg-patcher] release zip fetch failed for {url}: {e}"),
        }
    }

    // 3. Raw mirrors: the secondary fallback for whatever the zip did not supply.
    for name in PROXY_NAMES {
        let dest = proxy_dir.join(name);
        if !is_usable(&dest) {
            match fetch_from_sources(&bases, &proxy_rel(name), name, &progress) {
                Ok(bytes) => {
                    std::fs::write(&dest, &bytes)
                        .map_err(|e| format!("write {}: {e}", dest.display()))?;
                    downloaded.insert(name.to_string(), bytes.len());
                }
                Err(e) => eprintln!("[dlssg-patcher] mirror fetch failed for {name}: {e}"),
            }
        }
    }
    if !is_usable(&cache_ini) {
        match fetch_from_sources(&bases, ini_rel(), INI_NAME, &progress) {
            Ok(bytes) => {
                std::fs::write(&cache_ini, &bytes).map_err(|e| format!("write {}: {e}", cache_ini.display()))?;
                downloaded.insert(INI_NAME.to_string(), bytes.len());
            }
            Err(e) => eprintln!("[dlssg-patcher] mirror fetch failed for {INI_NAME}: {e}"),
        }
    }

    // 4. Local `files/` folders for anything still absent.
    if !cache_is_complete(&cache) {
        // A real release zip kept on disk is extracted exactly like a download.
        if let Some(zip_path) = find_local_zip(local_dirs) {
            match std::fs::read(&zip_path) {
                Ok(bytes) => match extract_release_zip(&bytes, &cache, false) {
                    Ok(_) => from_local = true,
                    Err(e) => eprintln!("[dlssg-patcher] local zip extract failed {}: {e}", zip_path.display()),
                },
                Err(e) => eprintln!("[dlssg-patcher] read {}: {e}", zip_path.display()),
            }
        }
        // The runtime tree never comes from the repo mirrors, so a local copy is the only
        // offline source besides the zip. Start clean: an older tree must never be merged.
        if !is_runtime_dir_usable(&cache.join(RUNTIME_DIR_NAME)) {
            if let Some(src) = find_local_runtime_dir(local_dirs) {
                let dst = cache.join(RUNTIME_DIR_NAME);
                let _ = std::fs::remove_dir_all(&dst);
                match copy_dir_recursive(&src, &dst) {
                    Ok(()) => from_local = true,
                    Err(e) => eprintln!("[dlssg-patcher] local runtime copy failed: {e}"),
                }
            }
        }
        // The existing per-file path: flat or under `proxy/`.
        for name in PROXY_NAMES {
            let dest = proxy_dir.join(name);
            if !is_usable(&dest) {
                if let Some(src) = find_local_file(local_dirs, name) {
                    if copy_local(&src, &dest) {
                        from_local = true;
                    }
                }
            }
        }
        if !is_usable(&cache_ini) {
            if let Some(src) = find_local_file(local_dirs, INI_NAME) {
                if copy_local(&src, &cache_ini) {
                    from_local = true;
                }
            }
        }
    }

    // 5. Version sidecar. The upstream INI dropped its "; Native x.y.z" header, so the local
    // version is kept in a small sidecar (version.txt) that survives refreshes. version.txt is
    // the source of truth: it is rewritten from the README (or the tag) on any refresh, while
    // the cached INI may still carry an old "; Native x.y.z" header from a previous snapshot.
    let mut version = cached_mod_version(&cache);
    if !downloaded.is_empty() || fetched_from_zip || (force && from_local) {
        if let Some(v) = version_from_readme_or_tag {
            version = Some(v.to_string());
        }
    }
    if let Some(v) = version.as_deref() {
        write_cached_version(&cache, v);
    }

    // 6. Report exactly what is absent, so the UI names the missing piece instead of
    // "missing: ". The runtime tree cannot come from the repo mirrors: the payload files are
    // tag-only and `sr/` is release-zip only, so if it is gone the cache stays incomplete and
    // says so, loudly.
    let mut missing = Vec::new();
    for name in PROXY_NAMES {
        if !is_usable(&proxy_dir.join(name)) {
            missing.push(name.to_string());
        }
    }
    if !is_usable(&cache_ini) {
        missing.push(INI_NAME.to_string());
    }
    if !is_runtime_dir_usable(&cache.join(RUNTIME_DIR_NAME)) {
        eprintln!(
            "[dlssg-patcher] the {RUNTIME_DIR_NAME} runtime tree is incomplete; a patch cannot work without {RUNTIME_REQUIRED:?}"
        );
        missing.push(RUNTIME_DIR_NAME.to_string());
    }

    let source = if !missing.is_empty() {
        FileSource::Missing(missing)
    } else if fetched_from_zip || !downloaded.is_empty() {
        FileSource::Github
    } else if from_local {
        FileSource::Local
    } else {
        FileSource::Cached
    };

    if let Some(cb) = &progress {
        cb(format!("Total: {} file(s) fetched", downloaded.len() + usize::from(fetched_from_zip)));
    }

    Ok(ModFiles {
        source,
        version,
        cache_dir: cache,
    })
}

/// A cached/bundled file is usable if it is non-trivial and, for a DLL, actually a
/// PE image (starts with the `MZ` DOS magic) — so an HTML error page or a truncated
/// download can never be mistaken for the proxy.
fn is_usable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if meta.len() <= 64 {
        return false;
    }
    let is_dll = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll"));
    if !is_dll {
        return true;
    }
    let mut head = [0u8; 2];
    match std::fs::File::open(p).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)) {
        Ok(()) => head == *b"MZ",
        Err(_) => false,
    }
}

/// Look for `<dir>/<file>` where `<dir>` is one of the local folders, either flat
/// (files/version.dll) or under `proxy/` (files/proxy/version.dll).
fn find_local_file(local_dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    for dir in local_dirs {
        if !dir.is_dir() {
            continue;
        }
        let flat = dir.join(name);
        if is_usable(&flat) {
            return Some(flat);
        }
        let nested = dir.join("proxy").join(name);
        if is_usable(&nested) {
            return Some(nested);
        }
    }
    None
}

fn copy_local(src: &Path, dest: &Path) -> bool {
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::copy(src, dest) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("[dlssg-patcher] local copy failed {} -> {}: {e}", src.display(), dest.display());
            false
        }
    }
}

/// Extract a release zip into `cache`. Returns the number of files written.
///
/// Routing is by path segment, not by a fixed prefix, so a future archive that wraps
/// everything in one folder still lands correctly:
///   * any path containing a `dlssg_sm86` segment -> `cache/dlssg_sm86/<rest>`
///   * a file named `dlssg_sm86.ini`              -> `cache/dlssg_sm86.ini`
///   * a file named like one of `PROXY_NAMES`     -> `cache/proxy/<name>`
///   * everything else (docs, README, LICENSE) is skipped
///
/// `include_sr = false` skips the `sr/` subtree: it is ~180 MB of the 209.7 MB payload and
/// `[SuperResolution] Mode` is `Off` by default.
pub fn extract_release_zip(bytes: &[u8], cache: &Path, include_sr: bool) -> Result<usize, String> {
    let proxy_dir = cache.join("proxy");
    std::fs::create_dir_all(&proxy_dir).map_err(|e| format!("create {}: {e}", proxy_dir.display()))?;
    // Start from a clean tree: a stale runtime folder from a 0.3.x-era or older-0.4.x
    // layout must never be merged with the one we are about to write.
    let _ = std::fs::remove_dir_all(cache.join(RUNTIME_DIR_NAME));

    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("open release zip: {e}"))?;

    let mut written = 0usize;
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| format!("open release zip entry {i}: {e}"))?;
        if e.is_dir() {
            continue;
        }
        // Zip entry names in the real archive use forward slashes, but normalise anyway so
        // a Windows-authored archive cannot smuggle a `..\\` path past the checks below.
        let name = e.name().replace('\\', "/");
        // Reject anything absolute (leading `/` or a drive letter) or traversing.
        if name.starts_with('/') || name.contains(':') {
            continue;
        }
        let segs: Vec<&str> = name.split('/').filter(|s| !s.is_empty()).collect();
        if segs.iter().any(|s| *s == "..") {
            continue;
        }
        let filename = segs.last().copied().unwrap_or("");
        let dest = if let Some(i) = segs.iter().position(|s| *s == RUNTIME_DIR_NAME) {
            let rest = &segs[i + 1..];
            if !include_sr && rest.first() == Some(&RUNTIME_SR_DIR) {
                continue;
            }
            if rest.is_empty() {
                continue; // a bare directory name is not a file we can place
            }
            cache.join(RUNTIME_DIR_NAME).join(rest.join("/"))
        } else if filename == INI_NAME {
            cache.join(INI_NAME)
        } else if PROXY_NAMES.contains(&filename) {
            proxy_dir.join(filename)
        } else {
            continue;
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::io::copy(&mut e, &mut std::fs::File::create(&dest)
            .map_err(|e| format!("create {}: {e}", dest.display()))?)
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cached/bundled file is trusted without a network round-trip, so a bad
    /// file (an HTML error page saved as `version.dll`) must be detected here: a
    /// DLL has to be a real PE image, not merely big enough.
    #[test]
    fn is_usable_rejects_non_pe_dll() {
        let dir = std::env::temp_dir().join(format!("dl_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let html = dir.join("version.dll");
        std::fs::write(&html, vec![b'<'; 4096]).unwrap();
        assert!(!is_usable(&html), "an HTML page named .dll must be rejected");

        let pe = dir.join("version2.dll");
        let mut bytes = b"MZ".to_vec();
        bytes.extend(std::iter::repeat(0u8).take(4094));
        std::fs::write(&pe, bytes).unwrap();
        assert!(is_usable(&pe), "a PE-magic DLL must be accepted");

        let ini = dir.join("dlssg_sm86.ini");
        std::fs::write(&ini, vec![b'a'; 4096]).unwrap();
        assert!(is_usable(&ini), "non-dll files only need a sane size");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pinned_release_hash_matches_known_versions_only() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let h = "5865360c3295038f0ff979d303bb7eb9437eec09fbc08686967d3b5d41d9c019";
        assert_eq!(pinned_release_hash(Some("0.4.0")), Some(h));
        assert_eq!(pinned_release_hash(Some("v0.4.0")), Some(h));
        assert_eq!(pinned_release_hash(Some("9.9.9")), None);
        assert_eq!(pinned_release_hash(None), None);
    }

    #[test]
    fn verify_release_zip_classifies_known_unknown_and_tampered() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let table: &[(&str, &str)] = &[("1.2.3", abc)];
        assert_eq!(verify_release_zip(table, Some("1.2.3"), b"abc"), ZipTrust::Trusted);
        assert_eq!(verify_release_zip(table, Some("v1.2.3"), b"abc"), ZipTrust::Trusted);
        assert_eq!(verify_release_zip(table, Some("1.2.3"), b"tampered"), ZipTrust::Mismatch);
        assert_eq!(verify_release_zip(table, Some("9.9.9"), b"abc"), ZipTrust::Unverified);
        assert_eq!(verify_release_zip(table, None, b"abc"), ZipTrust::Unverified);
    }

    /// The fetch order is a safety property: the immutable release tag must be
    /// tried before the moving `main` branch, with `main` only as a last resort.
    #[test]
    fn mirror_bases_puts_tag_first() {
        let with = mirror_bases(Some("0.3.5"));
        assert!(with[0].ends_with("/0.3.5/"), "{:?}", with[0]);
        assert!(with[1].contains("@0.3.5/"));
        assert!(with.iter().any(|u| u.contains("/main/")));
        let without = mirror_bases(None);
        assert_eq!(without.len(), 2);
        assert!(without.iter().all(|u| u.contains("/main/") || u.contains("@main/")));
    }

    #[test]
    fn sanitize_tag_rejects_paths() {
        assert_eq!(sanitize_tag("0.3.5"), Some("0.3.5"));
        assert_eq!(sanitize_tag("v1.2"), Some("v1.2"));
        assert_eq!(sanitize_tag("main"), None);
        assert_eq!(sanitize_tag("../../etc"), None);
        assert_eq!(sanitize_tag("0.3.5/x"), None);
    }

    #[test]
    fn proxy_rel_matches_the_0_4_0_layout() {
        assert_eq!(proxy_rel("version.dll"), "version.dll");
        assert_eq!(proxy_rel("winmm.dll"), "alternatives/winmm.dll");
        assert_eq!(proxy_rel("d3d12.dll"), "alternatives/d3d12.dll");
    }

    #[test]
    fn runtime_required_paths_are_the_real_0_4_0_payload() {
        assert_eq!(RUNTIME_REQUIRED.len(), 4);
        assert!(RUNTIME_REQUIRED.iter().all(|p| !p.contains("sr/")), "sr/ is opt-in, never required");
        assert!(RUNTIME_REQUIRED.contains(&"runtime/nvngx_dlssg.dll"));
    }

    #[test]
    fn body_limit_clears_the_0_4_0_release_zip() {
        // Measured size of dlssg-release-0.4.0-x64.zip. If the limit is ever lowered below
        // this, the release download fails mid-stream again.
        const ZIP_0_4_0: usize = 80_551_324;
        assert!(BODY_LIMIT > ZIP_0_4_0, "BODY_LIMIT {BODY_LIMIT} cannot hold {ZIP_0_4_0}");
    }

    #[test]
    fn newest_version_takes_the_highest_and_ignores_junk() {
        assert_eq!(newest_version(&[Some("0.3.5".into()), Some("0.4.0".into())]).as_deref(), Some("0.4.0"));
        assert_eq!(newest_version(&[None, Some("0.4.0".into())]).as_deref(), Some("0.4.0"));
        assert_eq!(newest_version(&[None, None]), None);
        // `main` and paths are not versions.
        assert_eq!(newest_version(&[Some("main".into()), Some("0.4.0".into())]).as_deref(), Some("0.4.0"));
        assert_eq!(newest_version(&[Some("main".into())]), None);
    }

    #[test]
    fn parse_version_from_ini_still_finds_the_legacy_native_header() {
        assert_eq!(parse_version_from_ini("; Native 0.2.4. Restart the game.\n"), Some("0.2.4".into()));
        // The 0.4.0 INI has no version line at all: None is the honest answer.
        assert_eq!(parse_version_from_ini("; DLSSG SM86 - frame generation for Ampere.\n"), None);
        // The header is not required to be in the first 5 lines.
        let late = format!("{}; Native 0.9.9\n", "[x]\n".repeat(8));
        assert_eq!(parse_version_from_ini(&late), Some("0.9.9".into()));
    }

    #[test]
    fn zip_urls_covers_both_tag_spellings_and_dedupes() {
        let u = zip_urls(Some("0.4.0"), Some("0.4.0"));
        assert_eq!(u[0], "https://github.com/sdli1995/dlssg_for_sm86/releases/download/0.4.0/dlssg-release-0.4.0-x64.zip");
        assert!(u.iter().any(|x| x.contains("/download/v0.4.0/")));
        assert_eq!(u.iter().filter(|x| x.contains("/download/0.4.0/")).count(), 1, "no duplicate URLs");
        // No usable version -> no URLs (never guess a path).
        assert!(zip_urls(None, None).is_empty());
        assert!(zip_urls(Some("main"), None).is_empty());
    }

    /// A fresh per-process temp dir, wiped first so a stale directory from an earlier
    /// run can never be mistaken for this run's output (mirrors the existing
    /// `is_usable_rejects_non_pe_dll` fixture's `dl_test_<pid>` scheme).
    ///
    /// The sequence number matters: tests run in parallel threads inside one process, and
    /// `is_usable_rejects_non_pe_dll` removes `dl_test_<pid>` when it finishes, so sharing
    /// that exact path would delete this test's files mid-extraction.
    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dl_test_{}_{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tiny_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write as _;
        use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};
        let mut w = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// `b"MZ"` padded to `n` bytes with zeros, so `is_usable`'s PE check accepts it.
    fn pe_bytes(n: usize) -> Vec<u8> {
        let mut v = b"MZ".to_vec();
        v.resize(n, 0);
        v
    }

    #[test]
    fn extract_routes_the_0_4_0_layout_and_skips_the_rest() {
        let dir = tmp();  // fresh temp dir
        let zip = tiny_zip(&[
            ("version.dll", b"MZ-version"),
            ("alternatives/winmm.dll", b"MZ-winmm"),
            ("dlssg_sm86.ini", b"[DLSSG]"),
            ("dlssg_sm86/runtime/nvngx_dlssg.dll", b"MZ-runtime"),
            ("dlssg_sm86/backend/sm86_backend.dll", b"MZ-backend"),
            ("dlssg_sm86/sr/sr_kernels.dll", b"MZ-sr"),
            ("docs/index.html", b"<html>"),
            ("README.md", b"readme"),
        ]);
        let n = extract_release_zip(&zip, &dir, false).unwrap();
        assert_eq!(n, 5, "proxy + ini + 3 runtime files; docs/README/sr skipped");
        assert!(dir.join("proxy/version.dll").is_file());
        assert!(dir.join("proxy/winmm.dll").is_file(), "alternatives/ maps to proxy/");
        assert!(dir.join("dlssg_sm86.ini").is_file());
        assert!(dir.join("dlssg_sm86/runtime/nvngx_dlssg.dll").is_file());
        assert!(dir.join("dlssg_sm86/backend/sm86_backend.dll").is_file());
        assert!(!dir.join("dlssg_sm86/sr").exists(), "sr/ must not land when include_sr = false");
        assert!(!dir.join("docs").exists());
        assert!(!dir.join("README.md").exists());
    }

    #[test]
    fn extract_rejects_traversal_and_accepts_sr_when_asked() {
        let dir = tmp();
        let zip = tiny_zip(&[
            ("../../evil.dll", b"MZ"),
            ("/abs/evil.dll", b"MZ"),
            ("..\\..\\win-evil.dll", b"MZ"),
            ("dlssg_sm86/sr/sr_kernels.dll", b"MZ-sr"),
        ]);
        let n = extract_release_zip(&zip, &dir, true).unwrap();
        assert_eq!(n, 1, "only the sr file is legitimate");
        assert!(dir.join("dlssg_sm86/sr/sr_kernels.dll").is_file(), "include_sr = true keeps sr/");
        assert!(!std::path::Path::new(&dir).join("evil.dll").exists());
    }

    #[test]
    fn cache_incomplete_until_the_runtime_tree_and_all_six_proxies_are_present() {
        let cache = tmp();
        assert!(!cache_is_complete(&cache));
        std::fs::create_dir_all(cache.join("proxy")).unwrap();
        for n in PROXY_NAMES { std::fs::write(cache.join("proxy").join(n), pe_bytes(4096)).unwrap(); }
        std::fs::write(cache.join(INI_NAME), b"x".repeat(4096)).unwrap();
        assert!(!cache_is_complete(&cache), "runtime tree still missing");
        for rel in RUNTIME_REQUIRED {
            let p = cache.join(RUNTIME_DIR_NAME).join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, pe_bytes(4096)).unwrap();
        }
        assert!(cache_is_complete(&cache));
        // Dropping any single payload file must make it incomplete again.
        std::fs::remove_file(cache.join(RUNTIME_DIR_NAME).join(RUNTIME_REQUIRED[2])).unwrap();
        assert!(!cache_is_complete(&cache));
    }

    #[test]
    fn a_0_3_5_style_cache_is_never_considered_complete() {
        // The old cache held one big proxy and no runtime tree at all.
        let cache = tmp();
        std::fs::create_dir_all(cache.join("proxy")).unwrap();
        std::fs::write(cache.join("proxy").join("version.dll"), pe_bytes(2_000_000)).unwrap();
        std::fs::write(cache.join(INI_NAME), b"; Native 0.3.5\n".to_vec()).unwrap();
        assert!(!cache_is_complete(&cache), "must force the 0.4.0 bundle fetch");
    }

    /// A silent producer must trip the stall window: a dead line never ends on its own.
    #[test]
    fn drain_with_stall_gives_up_on_a_silent_reader_naming_the_bytes() {
        let (tx, rx) = std::sync::mpsc::channel();
        // Hold the sender open for longer than the stall window without sending anything.
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            drop(tx);
        });
        let err = drain_with_stall(
            rx,
            Duration::from_millis(50),
            Duration::from_secs(5),
            ZIP_PROGRESS_STEP,
            &None,
        )
        .unwrap_err();
        assert!(err.contains("0 bytes"), "the byte count must be named: {err}");
        producer.join().unwrap();
    }

    /// Chunks inside the window are a healthy transfer; dropping the sender ends it.
    #[test]
    fn drain_with_stall_completes_when_chunks_keep_arriving_inside_the_window() {
        let (tx, rx) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            for i in 1..=3u64 {
                std::thread::sleep(Duration::from_millis(5));
                if tx.send(Ok((i * 1024, vec![i as u8; 1024]))).is_err() {
                    return;
                }
            }
            // Dropping the sender is the end of the body.
        });
        let bytes = drain_with_stall(
            rx,
            // A window wide enough that neither a slow CI nor a late thread start is a stall.
            Duration::from_secs(5),
            Duration::from_secs(30),
            ZIP_PROGRESS_STEP,
            &None,
        )
        .unwrap();
        assert_eq!(bytes.len(), 3 * 1024, "every chunk is kept");
        let expected: Vec<u8> = (1..=3u8).flat_map(|i| std::iter::repeat_n(i, 1024)).collect();
        assert_eq!(bytes, expected, "chunks are concatenated in order");
        producer.join().unwrap();
    }

    /// A partial read followed by silence must name what was already received.
    #[test]
    fn drain_with_stall_names_the_byte_count_after_a_partial_then_stalled_read() {
        let (tx, rx) = std::sync::mpsc::channel();
        // Queue the chunk before draining, so it is always in hand before the window opens.
        tx.send(Ok((4096u64, vec![7u8; 4096]))).unwrap();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            drop(tx);
        });
        let err = drain_with_stall(
            rx,
            Duration::from_millis(50),
            Duration::from_secs(5),
            ZIP_PROGRESS_STEP,
            &None,
        )
        .unwrap_err();
        assert!(err.contains("4096"), "the byte count received must be named: {err}");
        holder.join().unwrap();
    }

    /// A worker error is the real reason the transfer failed; never swallow it.
    #[test]
    fn drain_with_stall_propagates_a_worker_error() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Err("read body: boom".to_string())).unwrap();
        drop(tx);
        let err = drain_with_stall(
            rx,
            Duration::from_millis(50),
            Duration::from_secs(5),
            ZIP_PROGRESS_STEP,
            &None,
        )
        .unwrap_err();
        assert_eq!(err, "read body: boom");
    }

    /// A silent sender never trips the stall window, so the overall cap is the only way out.
    #[test]
    fn drain_with_stall_honours_the_overall_cap() {
        let (tx, rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            drop(tx);
        });
        let err = drain_with_stall(
            rx,
            // A stall window longer than the cap proves the cap, not the window, ended it.
            Duration::from_secs(5),
            Duration::from_millis(30),
            ZIP_PROGRESS_STEP,
            &None,
        )
        .unwrap_err();
        assert!(err.contains("cap"), "the cap must be named: {err}");
        holder.join().unwrap();
    }
}

