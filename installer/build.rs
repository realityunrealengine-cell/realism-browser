fn main() {
    let exe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/release/realism.exe");
    let exe = exe.canonicalize().expect("build the browser first: cargo build --release (in the project root)");
    println!("cargo:rustc-env=REALISM_EXE={}", exe.display());
    println!("cargo:rerun-if-changed={}", exe.display());
    println!("cargo:rerun-if-changed=setup.rc");
    // Keep the setup version identical to the browser version.
    let cargo = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml")).unwrap();
    let ver = cargo.lines().find_map(|l| l.strip_prefix("version = \"")).and_then(|v| v.split('"').next()).unwrap().to_string();
    println!("cargo:rustc-env=REALISM_VERSION={ver}");
    println!("cargo:rerun-if-changed=../Cargo.toml");
    embed_resource::compile("setup.rc", embed_resource::NONE).manifest_optional().unwrap();
}
