//! Build script: let this crate's test binary load Swift's back-deployable
//! runtime.
//!
//! On macOS, `nagori-platform-native` links the Apple AI bridge (via
//! `nagori-ai-apple`), whose Swift static library references
//! `@rpath/libswift_Concurrency.dylib`. A dependency build script's
//! `rustc-link-arg` does not propagate to dependents, so the crate's own unit-
//! test executable needs the system Swift library directory added as a runpath
//! — the same fix the `nagori` CLI and the desktop app apply for their binaries.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
