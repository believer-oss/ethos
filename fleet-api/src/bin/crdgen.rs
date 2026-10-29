//! Writes the CRD YAML for every kind into the crate's `crds/` directory, replacing what is there.
//!
//! Run with `cargo run -p fleet-api --features crdgen --bin crdgen`.

use std::fs;
use std::path::Path;

fn main() -> std::io::Result<()> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("crds");
    fs::create_dir_all(&dir)?;

    // Remove CRDs for kinds that no longer exist, so the directory matches the crate exactly.
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "yaml") {
            fs::remove_file(&path)?;
        }
    }

    for crd in fleet_api::render_crds() {
        let path = dir.join(&crd.file_name);
        fs::write(&path, crd.yaml)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
