// Puts the icon and the version into rig.exe when it is built on Windows.
fn main() {
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("packaging/windows/gantry.ico");
            res.set("ProductName", "Gantry");
            res.set("FileDescription", "Gantry: mods for your games");
            if let Err(e) = res.compile() {
                println!("cargo:warning=could not embed the icon: {e}");
            }
        }
    }
}
