use std::path::Path;

/// Result of an anti-cheat scan: what was found, and whether the scan managed to
/// finish. "Nothing found" only means "clean" when `incomplete` is false.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcScan {
    /// Marketing name of the detected system, if any.
    pub detected: Option<String>,
    /// The scan was cut short (the folder held more entries than the budget), so
    /// "no detection" is inconclusive rather than a definitive "clean".
    pub incomplete: bool,
}

impl AcScan {
    pub fn is_detected(&self) -> bool {
        self.detected.is_some()
    }

    pub fn detected_name(&self) -> Option<&str> {
        self.detected.as_deref()
    }

    /// Tooltip for the row's warning badge, or `None` when there is nothing to
    /// warn about. An incomplete scan also warns: "nothing found" then means
    /// "not looked at", which must not be shown as clean.
    pub fn badge_tooltip(&self) -> Option<String> {
        match (&self.detected, self.incomplete) {
            (Some(name), _) => Some(format!("{name} detected")),
            (None, true) => Some(
                "Anti-cheat scan was incomplete (very large install); a protected \
                 title may not have been detected."
                    .to_string(),
            ),
            (None, false) => None,
        }
    }
}

/// Entry budget for the directory walk. Normal installs are well under this.
const ENTRY_BUDGET: usize = 50_000;

/// How a signature is compared against a file/dir name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Match {
    /// The whole (lowercased) name equals the needle.
    Exact,
    /// The name starts with the needle (e.g. `beclient`).
    Prefix,
}

/// Anti-cheat artifacts found inside a game install dir: `(needle, display name,
/// match)`. Only systems whose install-dir files are confirmed appear here; the
/// kernel-only systems (Ricochet, Tencent ACE, Javelin) leave no reliably-named
/// file in the game folder, so they are covered by the catalog `anticheat` hint
/// (`detect_hinted`) rather than by guessing file names.
const SIGNATURES: &[(&str, &str, Match)] = &[
    ("easyanticheat", "Easy Anti-Cheat", Match::Prefix),
    ("start_protected_game.exe", "Easy Anti-Cheat", Match::Exact),
    ("battleye", "BattlEye", Match::Exact),
    ("beclient", "BattlEye", Match::Prefix),
    ("beservice", "BattlEye", Match::Prefix),
];

/// Display name of the first signature matching a lowercased file/dir name.
fn signature_for(name_lower: &str) -> Option<&'static str> {
    SIGNATURES.iter().find_map(|(needle, display, how)| {
        let hit = match how {
            Match::Exact => name_lower == *needle,
            Match::Prefix => name_lower.starts_with(needle),
        };
        hit.then_some(*display)
    })
}

/// Detect a kernel/anti-tamper anti-cheat system inside a game install folder.
///
/// This is a purely offline, name-based scan: installing the Frame Generation
/// mod into a protected title can get the account banned, so the UI warns before
/// patching. It is intentionally shallow (a couple of directory levels, bounded
/// entry count) so scanning hundreds of installed games stays fast.
pub fn detect(install_dir: &Path) -> AcScan {
    detect_with_budget(install_dir, ENTRY_BUDGET)
}

/// Like [`detect`], but the catalog can name the system outright.
///
/// Kernel anti-cheats (Ricochet, Tencent ACE, Javelin) put their files in
/// System32 rather than the game folder, so a name scan of the install dir can
/// never find them; for a catalogued title the answer comes from `games.json`.
/// A missing/blank hint means "unknown" and falls back to the file scan.
pub fn detect_hinted(install_dir: &Path, hint: Option<&str>) -> AcScan {
    match hint.map(str::trim).filter(|h| !h.is_empty()) {
        Some(name) => AcScan { detected: Some(name.to_string()), incomplete: false },
        None => detect(install_dir),
    }
}

/// Scan with an explicit entry budget. Tests use a small budget to force the
/// `incomplete` path without creating 50 000 files.
pub(crate) fn detect_with_budget(install_dir: &Path, budget: usize) -> AcScan {
    let mut stack = vec![(install_dir.to_path_buf(), 0usize)];
    let mut visited = 0usize;

    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            visited += 1;
            if visited > budget {
                return AcScan { detected: None, incomplete: true };
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();

            if let Some(detected) = signature_for(&name) {
                return AcScan { detected: Some(detected.to_string()), incomplete: false };
            }

            if depth < 2 {
                if let Ok(ft) = entry.file_type() {
                    if ft.is_dir() && !name.starts_with('.') {
                        stack.push((entry.path(), depth + 1));
                    }
                }
            }
        }
    }
    AcScan::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signature table is the single place that maps an install-dir artifact
    /// to a display name; these are the artifacts we can confirm appear in a game
    /// folder (kernel-only systems are covered by the catalog hint instead).
    #[test]
    fn signature_table_maps_canonical_artifacts() {
        assert_eq!(signature_for("easyanticheat_x64.dll"), Some("Easy Anti-Cheat"));
        assert_eq!(signature_for("easyanticheat_eos.dll"), Some("Easy Anti-Cheat"));
        assert_eq!(signature_for("start_protected_game.exe"), Some("Easy Anti-Cheat"));
        assert_eq!(signature_for("battleye"), Some("BattlEye"));
        assert_eq!(signature_for("beclient_x64.dll"), Some("BattlEye"));
        assert_eq!(signature_for("beservice_x64.exe"), Some("BattlEye"));
        assert_eq!(signature_for("game.exe"), None);
        assert_eq!(signature_for("eac_launcher.exe"), None, "needles are not substrings");
    }

    #[test]
    fn detects_eac_files() {
        let dir = std::env::temp_dir().join(format!("ac_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("EasyAntiCheat_x64.dll"), b"x").unwrap();
        assert!(detect(&dir).is_detected());
        assert_eq!(detect(&dir).detected_name(), Some("Easy Anti-Cheat"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_battleye_dir() {
        let dir = std::env::temp_dir().join(format!("ac_test_be_{}", std::process::id()));
        let be = dir.join("BattlEye");
        let _ = std::fs::create_dir_all(&be);
        std::fs::write(be.join("BEClient_x64.dll"), b"x").unwrap();
        assert_eq!(detect(&dir).detected_name(), Some("BattlEye"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_folder_has_none() {
        let dir = std::env::temp_dir().join(format!("ac_test_none_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("game.exe"), b"x").unwrap();
        let s = detect(&dir);
        assert!(!s.is_detected());
        assert!(!s.incomplete, "a small folder is scanned to completion");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder big enough to exhaust the budget must report `incomplete`, and that
    /// must never be mistaken for a detection (the UI shows a weaker warning).
    #[test]
    fn scan_reports_incomplete_at_cap_but_no_false_detection() {
        let dir = std::env::temp_dir().join(format!("ac_cap_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        for i in 0..5 {
            std::fs::write(dir.join(format!("f{i}.dat")), b"x").unwrap();
        }
        let s = detect_with_budget(&dir, 2);
        assert!(s.incomplete && s.detected.is_none());
        let s2 = detect_with_budget(&dir, 100);
        assert!(!s2.incomplete);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The catalog knows the protected title by name even when the install dir
    /// holds no recognizable file (kernel anti-cheats leave nothing in the game
    /// folder), so the hint must win over the name scan.
    #[test]
    fn catalog_hint_wins_over_the_file_scan() {
        let dir = std::env::temp_dir().join(format!("ac_hint_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("EasyAntiCheat_x64.dll"), b"x").unwrap();
        let s = detect_hinted(&dir, Some("Ricochet"));
        assert_eq!(s.detected_name(), Some("Ricochet"));
        assert!(!s.incomplete);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing or whitespace-only hint means "unknown": the file scan decides.
    #[test]
    fn blank_hint_falls_through_to_the_file_scan() {
        let dir = std::env::temp_dir().join(format!("ac_hint2_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("EasyAntiCheat_x64.dll"), b"x").unwrap();
        assert_eq!(detect_hinted(&dir, None).detected_name(), Some("Easy Anti-Cheat"));
        assert_eq!(detect_hinted(&dir, Some("   ")).detected_name(), Some("Easy Anti-Cheat"));
        // A blank hint must not fabricate a detection on a clean folder either.
        let clean = std::env::temp_dir().join(format!("ac_hint3_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&clean);
        std::fs::write(clean.join("game.exe"), b"x").unwrap();
        assert!(!detect_hinted(&clean, Some("")).is_detected());
        let _ = std::fs::remove_dir_all(&clean);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The warning badge must not read as a clean bill of health: an incomplete
    /// walk (huge install, budget exhausted) warns too, because "not found" then
    /// only means "not looked at".
    #[test]
    fn badge_tooltip_covers_detection_and_incomplete_scans() {
        assert_eq!(AcScan::default().badge_tooltip(), None);

        let hit = AcScan { detected: Some("Ricochet".into()), incomplete: false };
        assert_eq!(hit.badge_tooltip().as_deref(), Some("Ricochet detected"));

        let partial = AcScan { detected: None, incomplete: true };
        assert!(partial.badge_tooltip().unwrap().contains("incomplete"));

        // A concrete detection still wins over the incomplete flag.
        let both = AcScan { detected: Some("BattlEye".into()), incomplete: true };
        assert_eq!(both.badge_tooltip().as_deref(), Some("BattlEye detected"));
    }
}
