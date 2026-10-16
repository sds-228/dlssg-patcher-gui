fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        res.set("ProductName", "DLSSG Patcher");
        res.set("FileDescription", "Simple GUI for the dlssg_for_sm86 Frame Generation mod (RTX 20/30-series)");
        res.set("OriginalFilename", "dlssg-patcher.exe");
        res.set("LegalCopyright", "(c) 2026 sds-228");
        res.compile().expect("failed to embed Windows resources");
    }
}