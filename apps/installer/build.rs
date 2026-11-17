//! Packs the built Mini (target/release-mini/drxwnify-mini.exe, or DRX_MINI_EXE) into the installer, compressed:
//! xz with the x86 branch filter and LZMA2 at the strongest preset (22.6 MB → ~8 MB; deflate gave ~11 MB).

use std::io::Read;

fn main() {
    let exe = std::env::var("DRX_MINI_EXE").unwrap_or_else(|_| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        root.join("target/release-mini/drxwnify-mini.exe").display().to_string()
    });
    println!("cargo:rerun-if-changed={exe}");
    println!("cargo:rerun-if-env-changed=DRX_MINI_EXE");
    let bytes = std::fs::read(&exe)
        .unwrap_or_else(|e| panic!("build Mini first (scripts/build.ps1 cargo build -p drxwnify-mini --profile release-mini): {exe}: {e}"));
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("mini.xz");
    let mut opts = liblzma::stream::LzmaOptions::new_preset(9 | liblzma::stream::PRESET_EXTREME).unwrap();
    opts.dict_size(32 << 20);
    let mut filters = liblzma::stream::Filters::new();
    filters.x86();
    filters.lzma2(&opts);
    let stream = liblzma::stream::Stream::new_stream_encoder(&filters, liblzma::stream::Check::Crc32).unwrap();
    let mut packed = Vec::new();
    liblzma::read::XzEncoder::new_stream(&bytes[..], stream).read_to_end(&mut packed).unwrap();
    std::fs::write(out, packed).unwrap();
}
