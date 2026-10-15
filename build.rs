fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileVersion", "0.1.0");
        res.set("ProductVersion", "0.1.0");
        res.set("ProductName", "DLSSG Patcher");
        res.set("FileDescription", "Simple GUI for the dlssg_for_sm86 Frame Generation mod (RTX 20/30-series)");
        res.set("OriginalFilename", "dlssg-patcher.exe");
        res.set("LegalCopyright", "(c) 2026 sds-228");
        res.compile().expect("failed to embed Windows resources");
    }
}