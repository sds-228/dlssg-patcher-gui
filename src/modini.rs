//! Text-preserving INI editing for the upstream `dlssg_sm86.ini`.
//!
//! The upstream INI ships with comments and a fixed key order. Rewriting it
//! wholesale would throw away both, so this module edits only the lines it is
//! asked to and leaves everything else byte-identical.

/// A parsed INI document that remembers its line endings.
#[derive(Debug, Clone)]
pub struct IniDoc {
    lines: Vec<String>,
    eol: &'static str,
    trailing_eol: bool,
}

impl IniDoc {
    /// Parse `text`, remembering CRLF vs LF and whether it ended with a newline.
    pub fn parse(text: &str) -> Self {
        let eol = match text.find('\n') {
            Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
            _ => "\n",
        };
        Self {
            lines: text.lines().map(str::to_string).collect(),
            eol,
            trailing_eol: text.ends_with('\n'),
        }
    }

    /// Last value for `key` in `section`; matching ignores ASCII case.
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        let mut current: Option<&str> = None;
        let mut found = None;
        for line in &self.lines {
            if let Some(name) = section_name(line) {
                current = Some(name);
                continue;
            }
            if current.is_some_and(|s| s.eq_ignore_ascii_case(section)) {
                if let Some((k, v)) = key_value(line) {
                    if k.eq_ignore_ascii_case(key) {
                        found = Some(v);
                    }
                }
            }
        }
        found
    }

    /// Set `key` in `section`, preserving every other line.
    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        let header = self
            .lines
            .iter()
            .position(|l| section_name(l).is_some_and(|s| s.eq_ignore_ascii_case(section)));
        let Some(h) = header else {
            self.append_section(section, key, value);
            return;
        };

        // The section body ends at the next header (any section) or the file end.
        let end = self.lines[h + 1..]
            .iter()
            .position(|l| section_name(l).is_some())
            .map(|p| h + 1 + p)
            .unwrap_or(self.lines.len());

        // Update the first duplicate in place; later duplicates stay untouched.
        // Keep the existing key's spelling and spacing (only the value changes).
        for line in self.lines.iter_mut().take(end).skip(h + 1) {
            if key_value(line).is_some_and(|(k, _)| k.eq_ignore_ascii_case(key)) {
                if let Some(eq) = line.find('=') {
                    let prefix = &line[..eq];
                    *line = format!("{prefix}={value}");
                }
                return;
            }
        }

        // Insert after the section's last key, or directly under its header.
        let at = (h + 1..end)
            .rev()
            .find(|&i| key_value(&self.lines[i]).is_some())
            .map(|i| i + 1)
            .unwrap_or(h + 1);
        self.lines.insert(at, format!("{key}={value}"));
    }

    /// Render the document with its original line-ending style.
    pub fn render(&self) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let mut out = self.lines.join(self.eol);
        if self.trailing_eol {
            out.push_str(self.eol);
        }
        out
    }

    fn append_section(&mut self, section: &str, key: &str, value: &str) {
        if self.lines.last().is_some_and(|l| !l.trim().is_empty()) {
            self.lines.push(String::new());
        }
        self.lines.push(format!("[{section}]"));
        self.lines.push(format!("{key}={value}"));
        self.trailing_eol = true;
    }
}

/// `[Section]` -> `Section`, ignoring surrounding whitespace.
fn section_name(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.len() >= 2 && t.starts_with('[') && t.ends_with(']') {
        Some(t[1..t.len() - 1].trim())
    } else {
        None
    }
}

/// `Key = value` -> `(Key, value)`, skipping comments and blank lines.
fn key_value(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start();
    if t.starts_with(';') || t.starts_with('#') {
        return None;
    }
    let (k, v) = line.split_once('=')?;
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    Some((k, v.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_and_render(mut d: IniDoc, section: &str, key: &str, value: &str) -> String {
        d.set(section, key, value);
        d.render()
    }

    #[test]
    fn round_trip_preserves_lf_crlf_and_missing_final_newline() {
        for text in [
            "; c\n[DLSSG]\nPreset=A\n\n[SuperResolution]\nMode=Off\n",
            "; c\r\n[DLSSG]\r\nPreset=A\r\n",
            "; c\n[DLSSG]\nPreset=A",
        ] {
            assert_eq!(IniDoc::parse(text).render(), text);
        }
    }

    #[test]
    fn get_is_case_insensitive_and_last_duplicate_wins() {
        let d = IniDoc::parse("[dlssg]\nPreset=A\nPRESET=B\n");
        assert_eq!(d.get("DLSSG", "preset"), Some("B"));
        assert_eq!(d.get("Dlssg", "Preset"), Some("B"));
        assert_eq!(d.get("DLSSG", "missing"), None);
    }

    #[test]
    fn set_updates_first_duplicate_and_keeps_comments_and_order() {
        let d = IniDoc::parse("[DLSSG]\n; keep me\nPreset=A\nPreset=C\n");
        let out = set_and_render(d, "DLSSG", "preset", "B");
        assert_eq!(out, "[DLSSG]\n; keep me\nPreset=B\nPreset=C\n");
    }

    #[test]
    fn set_inserts_a_missing_key_inside_its_section() {
        let d = IniDoc::parse("[DLSSG]\nPreset=A\n[Other]\nX=1\n");
        let out = set_and_render(d, "DLSSG", "Optimized", "2");
        assert_eq!(out, "[DLSSG]\nPreset=A\nOptimized=2\n[Other]\nX=1\n");
    }

    #[test]
    fn set_appends_a_missing_section() {
        let d = IniDoc::parse("[DLSSG]\nPreset=A\n");
        let out = set_and_render(d, "XeSSFG", "UseHudless", "Auto");
        assert_eq!(out, "[DLSSG]\nPreset=A\n\n[XeSSFG]\nUseHudless=Auto\n");
    }
}
