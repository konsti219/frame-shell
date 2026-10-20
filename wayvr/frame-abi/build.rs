fn main() {
    println!("cargo::rerun-if-changed=include/frame_abi.h");
    let bindings = bindgen::Builder::default()
        .header("include/frame_abi.h")
        .allowlist_file(".*frame_abi\\.h")
        .rust_edition(bindgen::RustEdition::Edition2024)
        .derive_default(true)
        .generate()
        .expect("frame_abi.h bindings");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("frame_abi.rs");
    bindings.write_to_file(out).expect("writing frame_abi.rs");
}
