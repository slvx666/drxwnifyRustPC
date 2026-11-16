//! Packs the built Mini (target/release-mini/drxwnify-mini.exe, or DRX_MINI_EXE) into the installer, compressed.
use std::io::Write;

fn main() {
    let exe = std::env::var("DRX_MINI_EXE").unwrap_or_else(|_| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        root.join("target/release-mini/drxwnify-mini.exe").display().to_string()
    });
    println!("cargo:rerun-if-changed={exe}");
    println!("cargo:rerun-if-env-changed=DRX_MINI_EXE");
    let bytes = std::fs::read(&exe)
        .unwrap_or_else(|e| panic!("build Mini first (scripts/build.ps1 cargo build -p drxwnify-mini --profile release-mini): {exe}: {e}"));
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("mini.gz");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&bytes).unwrap();
    std::fs::write(out, gz.finish().unwrap()).unwrap();
}
