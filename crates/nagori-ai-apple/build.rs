//! Build script for `nagori-ai-apple`.
//!
//! On macOS it compiles the bundled Swift package (which links the
//! `FoundationModels` / Translation / `NaturalLanguage` frameworks) into a
//! static library and links it into the host crate. On every other platform it
//! is a no-op so the crate still compiles as a plain-Rust workspace member (mock
//! fixtures, longest-common-prefix delta, and the simulated stream driver all
//! build without Swift).

fn main() {
    // The `#[cfg(target_os = ...)]` here is evaluated for the *host* (build
    // scripts run on the host), which also gates the `swift-rs` build
    // dependency. The inner `CARGO_CFG_TARGET_OS` check is the *target*, so a
    // macOS host cross-compiling for Linux never links Swift into that binary.
    #[cfg(target_os = "macos")]
    link_swift();
}

#[cfg(target_os = "macos")]
fn link_swift() {
    use swift_rs::SwiftLinker;

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    // Match the minimum deployment target the app ships with
    // (tauri.conf.json `minimumSystemVersion: "26.0"`). The Swift package must
    // declare the same `.macOS("26.0")` platform or `import FoundationModels`
    // links but fails to load at runtime.
    SwiftLinker::new("26.0")
        .with_package("nagori_apple", "./swift")
        .link();

    // libswift_Concurrency.dylib is back-deployable; SwiftLinker leaves it as
    // an @rpath reference, so add the system Swift libraries directory as a
    // runpath for the host binary.
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    // swift-rs only emits libswiftFoundation; the Foundation framework itself
    // must be linked so the bridged Obj-C classes register at runtime.
    println!("cargo:rustc-link-lib=framework=Foundation");
    // Frameworks referenced by the Swift bridge.
    println!("cargo:rustc-link-lib=framework=FoundationModels");
    println!("cargo:rustc-link-lib=framework=Translation");
    println!("cargo:rustc-link-lib=framework=NaturalLanguage");

    // Rebuild when the Swift sources change.
    println!("cargo:rerun-if-changed=swift/Sources");
    println!("cargo:rerun-if-changed=swift/Package.swift");
}
