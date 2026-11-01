//! Embeds the application icon and version information in `bb.exe`. Other
//! platforms have no icon for a command-line binary, so nothing happens there.

fn main() {
    println!("cargo:rerun-if-changed=../desktop/icons/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../desktop/icons/icon.ico")
        .set("ProductName", "BucketBridge")
        .set(
            "FileDescription",
            "bb, the BucketBridge command-line S3 client",
        );
    if let Err(error) = resource.compile() {
        panic!("could not embed the Windows icon of bb.exe: {error}");
    }
}
