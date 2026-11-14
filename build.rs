//! Link settings that keep `notch.exe --hook` fast.
//!
//! The same exe is the island and the hook helper. The helper only needs kernel32, so the
//! graphics, shell, network and COM libraries are delay-loaded: Windows maps them the
//! first time one of their functions is called (which the helper never does) instead of
//! at process start.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    for dll in [
        "d2d1.dll",
        "dwrite.dll",
        "dwmapi.dll",
        "winhttp.dll",
        "powrprof.dll",
        "shell32.dll",
        "ole32.dll",
        "oleaut32.dll",
        "combase.dll",
        "user32.dll",
        "gdi32.dll",
        "wtsapi32.dll",
        "ws2_32.dll",
        "advapi32.dll",
        "dbghelp.dll",
        "api-ms-win-core-winrt-l1-1-0.dll",
        "api-ms-win-shcore-scaling-l1-1-1.dll",
    ] {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
    }
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
}
