//! Writes `include/accessor.h` from the `extern "C"` functions in `src/lib.rs`, so the header can
//! never drift from the code.

use std::env;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    let config = match cbindgen::Config::from_file(dir.join("cbindgen.toml")) {
        Ok(c) => c,
        Err(e) => {
            println!("cargo:warning=cannot read cbindgen.toml: {e}");
            return;
        }
    };
    match cbindgen::Builder::new().with_crate(&dir).with_config(config).generate() {
        Ok(bindings) => {
            let out = dir.join("include");
            let _ = std::fs::create_dir_all(&out);
            bindings.write_to_file(out.join("accessor.h"));
        }
        Err(e) => println!("cargo:warning=cannot generate accessor.h: {e}"),
    }
}
