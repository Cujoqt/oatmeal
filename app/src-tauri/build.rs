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
        // Emit for every linked artifact (bin, test, example) so `cargo test`
        // binaries also find the Swift runtime, not just the app binary.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

        // ggml's Metal backend is Objective-C and uses `@available(...)`, which
        // clang lowers to `___isPlatformVersionAtLeast` — a compiler-rt builtin.
        // rustc links with `-nodefaultlibs`, so clang's runtime library is never
        // pulled in and the symbol is undefined at link time. Link it explicitly.
        if let Some(dir) = clang_runtime_dir() {
            if std::path::Path::new(&dir).join("libclang_rt.osx.a").exists() {
                println!("cargo:rustc-link-search=native={dir}");
                println!("cargo:rustc-link-lib=static=clang_rt.osx");
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn clang_runtime_dir() -> Option<String> {
        let out = std::process::Command::new("clang")
            .arg("-print-runtime-dir")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let dir = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (!dir.is_empty()).then_some(dir)
    }

    tauri_build::build()
}
