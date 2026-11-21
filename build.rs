// Embeds assets/logo.ico as the .exe icon (Windows only). Never fails the build.
fn main() {
    println!("cargo:rerun-if-changed=assets/logo.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").map(|o| o == "windows").unwrap_or(false)
        && std::path::Path::new("assets/logo.ico").exists()
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/logo.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed exe icon: {e}");
        }
    }
}
