//! Build script: make the binary able to load Swift's back-deployable runtime.
//!
//! On macOS, `nagori` links the Apple AI bridge transitively (via
//! `nagori-platform-native` → `nagori-ai-apple`). That Swift static library
//! references `@rpath/libswift_Concurrency.dylib`. A dependency build script's
//! `rustc-link-arg` does not propagate to the final binary, so the system Swift
//! library directory must be added as a runpath here, in the binary crate.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
