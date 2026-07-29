fn main() {
    // rpath is only meaningful on macOS; skip on other platforms.
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    }
}
