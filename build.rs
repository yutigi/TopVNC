fn main() {
    // ScreenCaptureKit is missing before macOS 12.3. Weak linking lets the
    // app start there; the server checks the version before using it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-weak_framework,ScreenCaptureKit");
    }
}
