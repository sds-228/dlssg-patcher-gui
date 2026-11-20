use std::ffi::OsStr;
use std::path::PathBuf;

fn system32_under(root: Option<&OsStr>, name: &str) -> PathBuf {
    match root {
        Some(r) => PathBuf::from(r).join("System32").join(name),
        None => PathBuf::from(name),
    }
}

/// Absolute path to a System32 tool (`%SystemRoot%\System32\<name>`), falling
/// back to the bare name if the environment is unusual.
///
/// System tools are launched by full path so a same-named executable earlier on
/// `PATH` (a hijack in the working directory or a user's bin folder) cannot be
/// run instead.
pub fn system32(name: &str) -> PathBuf {
    system32_under(std::env::var_os("SystemRoot").as_deref(), name)
}

/// Absolute path to Windows Explorer. Explorer lives in `%SystemRoot%` (not
/// `System32`), so resolve it there and fall back to the bare name if needed.
pub fn explorer() -> PathBuf {
    match std::env::var_os("SystemRoot") {
        Some(r) => {
            let root = PathBuf::from(r);
            let p = root.join("explorer.exe");
            if p.is_file() {
                p
            } else {
                root.join("System32").join("explorer.exe")
            }
        }
        None => PathBuf::from("explorer.exe"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_tools_under_system32() {
        let p = system32_under(Some(std::ffi::OsStr::new(r"C:\Windows")), "reg.exe");
        assert_eq!(p, PathBuf::from(r"C:\Windows").join("System32").join("reg.exe"));
    }

    #[test]
    fn falls_back_to_the_bare_name_without_a_system_root() {
        assert_eq!(system32_under(None, "reg.exe"), PathBuf::from("reg.exe"));
    }
}
