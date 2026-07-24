fn main() {
    // The `screencapturekit` crate (and its apple-* deps) are Swift-based and
    // auto-link the Swift runtime compatibility shims (swiftCompatibility56, …).
    // Those static libs ship with the Command Line Tools, but the linker only
    // searches the full-Xcode toolchain path by default. When full Xcode is
    // absent, point it at the CLT Swift lib dir so the link resolves.
    #[cfg(target_os = "macos")]
    {
        for dir in [
            "/Library/Developer/CommandLineTools/usr/lib/swift/macosx",
            "/Library/Developer/CommandLineTools/usr/lib/swift_static/macosx",
        ] {
            if std::path::Path::new(dir).exists() {
                println!("cargo:rustc-link-search=native={dir}");
            }
        }
        // At runtime the Swift-based deps load `@rpath/libswift_*.dylib`. Add the
        // OS Swift runtime dir as an rpath so dyld resolves them (served from the
        // shared cache) — without this the binary aborts on launch with
        // "Library not loaded: @rpath/libswift_Concurrency.dylib".
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,/usr/lib/swift");
    }

    tauri_build::build()
}
