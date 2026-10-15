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
}

/// Entry budget for the directory walk. Normal installs are well under this.
const ENTRY_BUDGET: usize = 50_000;

/// Detect a kernel/anti-tamper anti-cheat system inside a game install folder.
///
/// This is a purely offline, name-based scan: installing the Frame Generation
/// mod into a protected title can get the account banned, so the UI warns before
/// patching. It is intentionally shallow (a couple of directory levels, bounded
/// entry count) so scanning hundreds of installed games stays fast.
pub fn detect(install_dir: &Path) -> AcScan {
    detect_with_budget(install_dir, ENTRY_BUDGET)
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

            if name.starts_with("easyanticheat") || name == "start_protected_game.exe" {
                return AcScan { detected: Some("Easy Anti-Cheat".to_string()), incomplete: false };
            }
            if name == "battleye" || name.starts_with("beclient") || name.starts_with("beservice") {
                return AcScan { detected: Some("BattlEye".to_string()), incomplete: false };
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
}
