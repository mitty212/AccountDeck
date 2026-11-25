fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-bin=accountdeck=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=accountdeck=/MANIFESTINPUT:{}\\app.manifest",
            std::env::var("CARGO_MANIFEST_DIR").unwrap()
        );
    }
}
