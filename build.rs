fn main() {
    println!("cargo:rerun-if-changed=realism.rc");
    println!("cargo:rerun-if-changed=realism.manifest");
    println!("cargo:rerun-if-changed=assets/realism.ico");
    embed_resource::compile("realism.rc", embed_resource::NONE).manifest_optional().unwrap();
}
