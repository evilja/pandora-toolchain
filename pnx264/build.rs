use std::env;
use std::path::PathBuf;

// libx264 is found in this order:
//   PNX264_LIB_DIR / PNX264_INCLUDE_DIR   — the forked build, once there is one
//   whatever the linker already sees      — the distro libx264, used for stock-parity tests
// PNX264_STATIC=1 links libx264.a instead of the shared object, which is what the forked
// encoder will want so a Pandora build never picks up a system x264 by accident.
fn main() {
    println!("cargo:rerun-if-changed=csrc/pnx264.c");
    println!("cargo:rerun-if-changed=csrc/pnx264.h");
    println!("cargo:rerun-if-env-changed=PNX264_LIB_DIR");
    println!("cargo:rerun-if-env-changed=PNX264_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=PNX264_STATIC");

    let mut build = cc::Build::new();
    build.file("csrc/pnx264.c").include("csrc");

    if let Ok(dir) = env::var("PNX264_INCLUDE_DIR") {
        build.include(PathBuf::from(dir));
    }
    build.compile("pnx264");

    if let Ok(dir) = env::var("PNX264_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
        // Without this, rebuilding the fork leaves cargo convinced nothing changed and the
        // old libx264.a stays statically linked into the binary — which silently invalidates
        // any measurement taken right after a fork change.
        for lib in ["libx264.a", "libx264.so"] {
            let p = PathBuf::from(&dir).join(lib);
            if p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
    }
    if let Ok(dir) = env::var("PNX264_INCLUDE_DIR") {
        println!("cargo:rerun-if-changed={}", PathBuf::from(dir).join("x264.h").display());
    }
    let kind = if env::var("PNX264_STATIC").as_deref() == Ok("1") { "static" } else { "dylib" };
    println!("cargo:rustc-link-lib={kind}=x264");
    // A static libx264 pulls in libm and pthread, which the shared object would have carried
    // its own DT_NEEDED for.
    if kind == "static" {
        println!("cargo:rustc-link-lib=dylib=m");
        println!("cargo:rustc-link-lib=dylib=pthread");
    }
}
