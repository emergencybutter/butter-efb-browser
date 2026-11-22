//! Embeds the EFB Community package (efb/Packages/buttersim-efb-browser, made by `node efb/pack.js`)
//! into the executable so that `--install` can deploy it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let pkg = manifest.join("..").join("efb").join("Packages").join("buttersim-efb-browser");
    println!("cargo:rerun-if-changed={}", pkg.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut files = Vec::new();
    walk(&pkg, &mut files);
    files.sort();
    if files.is_empty() {
        println!(
            "cargo:warning=EFB package not found at {} (run `node efb/pack.js` first): --install will only register the companion",
            pkg.display()
        );
    }

    let mut code = String::from("pub static EFB_PACKAGE: &[(&str, &[u8])] = &[\n");
    for f in &files {
        let abs = fs::canonicalize(f).unwrap();
        let rel = f.strip_prefix(&pkg).unwrap().to_string_lossy().replace('\\', "/");
        let abs = abs.to_string_lossy();
        let abs = abs.strip_prefix(r"\\?\").unwrap_or(&abs);
        writeln!(code, "    ({rel:?}, include_bytes!({abs:?})),").unwrap();
    }
    code.push_str("];\n");
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("efb_package.rs"), code).unwrap();
}
