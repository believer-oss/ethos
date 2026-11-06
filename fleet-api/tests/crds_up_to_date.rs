//! Fails when the committed CRD YAML under `crds/` doesn't match the types.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[test]
fn committed_crds_match_the_types() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("crds");
    let committed: BTreeMap<String, String> = fs::read_dir(&dir)
        .expect("crds/ exists")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "yaml"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&path).unwrap())
        })
        .collect();
    let generated: BTreeMap<String, String> = fleet_api::render_crds()
        .into_iter()
        .map(|crd| (crd.file_name, crd.yaml))
        .collect();

    assert!(
        committed == generated,
        "fleet-api/crds/ is stale; regenerate it with \
         `cargo run -p fleet-api --features crdgen --bin crdgen` and commit the result"
    );
}
