use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=RENDER_BRIDGE_DOCS_REF");
    let docs_ref = env::var("RENDER_BRIDGE_DOCS_REF").unwrap_or_else(|_| "main".into());
    assert!(
        docs_ref == "main"
            || (docs_ref.len() == 40 && docs_ref.bytes().all(|b| b.is_ascii_hexdigit())),
        "The Render Bridge documentation reference must be main or a full Git commit SHA."
    );
    println!(
        "cargo:rustc-env=RENDER_BRIDGE_DOCS_REF={}",
        docs_ref.to_ascii_lowercase()
    );

    println!("cargo:rerun-if-changed=assets/render-bridge.ico");
    println!("cargo:rerun-if-changed=assets/render-bridge.res");
    println!("cargo:rerun-if-changed=build.rs");

    // A compiled Windows resource is architecture-independent. Regenerate it
    // in assets with the Windows SDK: rc /nologo /fo render-bridge.res render-bridge.rc
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest path"));
        println!(
            "cargo:rustc-link-arg={}",
            root.join("assets/render-bridge.res").display()
        );
    }
}
