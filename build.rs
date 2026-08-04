fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("icon.ico");
        // The app does not track its own version: clear the version fields.
        res.set("FileVersion", "");
        res.set("ProductVersion", "");
        res.compile().expect("failed to compile Windows resources");
        println!("cargo:rerun-if-changed=icon.ico");
    }
}
