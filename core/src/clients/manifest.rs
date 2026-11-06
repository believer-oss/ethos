//! Build manifests, `<artifact bucket>/argo-artifacts/manifests/<sha>.json`, written by the build
//! pipeline. Friendshipper reads the two parts it shows before a promotion: the backend part
//! versions the build carries (`backend_version`), and the map IDs it has levels for
//! (`maps.levelAssets`), the same fields the fleet coordinator reads.

use std::collections::BTreeSet;

use fleet_api::BackendParts;
use serde::{Deserialize, Serialize};

/// The manifest's key in the artifact bucket.
pub fn manifest_key(sha: &str) -> String {
    format!("argo-artifacts/manifests/{sha}.json")
}

/// What Friendshipper uses from a build manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildManifest {
    pub backend: BackendParts,
    /// `maps.levelAssets` keys with a non-empty level, sorted. Empty for a build without them.
    pub map_ids: Vec<String>,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    backend_version: RawBackend,
    /// Read leniently: a malformed maps section reads as no maps, as the coordinator does.
    #[serde(default)]
    maps: Option<serde_json::Value>,
}

/// The manifest writer uses `jq` on the backend's healthcheck, so a missing part is `null`.
#[derive(Deserialize, Default)]
struct RawBackend {
    platform: Option<String>,
    portal: Option<String>,
    content: Option<String>,
    config: Option<String>,
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Parses a manifest's body.
pub fn parse(body: &[u8]) -> Result<BuildManifest, serde_json::Error> {
    let raw: Raw = serde_json::from_slice(body)?;
    let b = raw.backend_version;
    let map_ids: BTreeSet<String> = raw
        .maps
        .as_ref()
        .and_then(|m| m.get("levelAssets"))
        .and_then(|a| a.as_object())
        .map(|assets| {
            assets
                .iter()
                .filter(|(id, level)| {
                    !id.is_empty() && level.as_str().is_some_and(|l| !l.trim().is_empty())
                })
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default();
    Ok(BuildManifest {
        backend: BackendParts {
            platform: non_empty(b.platform),
            portal: non_empty(b.portal),
            content: non_empty(b.content),
            config: non_empty(b.config),
        },
        map_ids: map_ids.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_backend_parts_and_map_ids() {
        let body = br#"{
            "artifacts": {"windows_client": {"phase": "Succeeded"}},
            "backend_version": {
                "platform": "2026-11-01-10-00-00_1.2_main_aaaaaaaa",
                "portal": null,
                "content": "",
                "config": "2026-11-02-10-00-00_1.2_main_bbbbbbbb"
            },
            "maps": {"levelAssets": {"Docks": "/Game/L_Docks", "Arena": "/Game/L_Arena", "Empty": ""}}
        }"#;
        let manifest = parse(body).unwrap();
        assert_eq!(
            manifest.backend,
            BackendParts {
                platform: Some("2026-11-01-10-00-00_1.2_main_aaaaaaaa".into()),
                portal: None,
                content: None,
                config: Some("2026-11-02-10-00-00_1.2_main_bbbbbbbb".into()),
            }
        );
        assert_eq!(manifest.map_ids, vec!["Arena", "Docks"]);
    }

    #[test]
    fn an_old_manifest_without_maps_or_backend_has_neither() {
        let manifest = parse(br#"{"artifacts": {}, "maps": "not an object"}"#).unwrap();
        assert_eq!(manifest, BuildManifest::default());
        assert!(parse(b"{not json").is_err());
        assert_eq!(
            manifest_key("0123456789abcdef0123456789abcdef01234567"),
            "argo-artifacts/manifests/0123456789abcdef0123456789abcdef01234567.json"
        );
    }
}
