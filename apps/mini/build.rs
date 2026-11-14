fn main() {
    slint_build::compile("ui/app.slint").expect("compile Slint UI");
    // The app icon as resource 1 (Explorer, taskbar, tray).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=mini.rc");
        println!("cargo:rerun-if-changed=../../assets/icon.ico");
        embed_resource::compile("mini.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("embed the app icon");
    }
}
