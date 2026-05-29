fn main() {
    // On macOS the app links the Apple AI bridge transitively (via
    // `nagori-platform-native` → `nagori-ai-apple`), whose Swift static library
    // references `@rpath/libswift_Concurrency.dylib`. A dependency build
    // script's `rustc-link-arg` does not propagate to the final binary, so add
    // the system Swift library directory as a runpath here.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    tauri_build::build();
}
