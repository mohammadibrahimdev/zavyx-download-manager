// Embeds the Zavyx DM icon and version info into the Windows executable.
fn main() {
    println!("cargo:rerun-if-changed=../../assets/zavyx-dm.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/zavyx-dm.ico")
            .set("ProductName", "Zavyx Download Manager")
            .set("FileDescription", "Zavyx Download Manager")
            .set("CompanyName", "Mohammad Ibrahim")
            .set("LegalCopyright", "MIT License - github.com/mohammadibrahimdev");
        res.compile().expect("failed to embed Windows icon resource");
    }
}
