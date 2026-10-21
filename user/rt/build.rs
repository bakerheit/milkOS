//! Put the user linker script on the link search path so every program's `-Tuser.ld`
//! (user/.cargo/config.toml) finds it.
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::copy("user.ld", out.join("user.ld")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=user.ld");
}
