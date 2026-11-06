//! Names both operators derive: releases, Fleets, map slugs, step workflows and level URLs.

use sha2::{Digest, Sha256};

use crate::promotion::{is_full_sha, WORKFLOW_NAME_PREFIX};

/// Longest shard name that can have Fleets: `<shard>-<sha8>-<map slug>` has to fit
/// [`FLEET_NAME_MAX_LEN`] with room for a [`MAP_SLUG_MAX_LEN`] slug. A name this short is never cut
/// in Promotion names either (see `Promotion::request`).
pub const FLEET_SHARD_NAME_MAX_LEN: usize = 15;

/// Characters of the version in release and Fleet names, as the build pipeline's `--short=8`.
pub const SHORT_SHA_LEN: usize = 8;

/// Kubernetes object names that appear in label values (the Fleet's `agones.dev/fleet`, the
/// `fleet.believer.dev/release` label) are at most 63 characters.
const NAME_MAX_LEN: usize = 63;

/// What a generated name adds to its prefix: `-` and five random characters.
const GENERATED_SUFFIX_LEN: usize = 6;

/// Longest Fleet name. Agones names a Fleet's GameServerSets `<fleet>-xxxxx` and their GameServers
/// `<set>-xxxxx` with `generateName`, whose base Kubernetes cuts short rather than pass 63
/// characters. At this length nothing is cut: every GameServer is `<fleet>-xxxxx-xxxxx`, so the
/// names stay predictable and readable.
pub const FLEET_NAME_MAX_LEN: usize = NAME_MAX_LEN - 2 * GENERATED_SUFFIX_LEN;

/// Pattern for a map ID: the row name of a map in the game's map table. It is used as the `believer.dev/map` label value, so it
/// must be a valid label value.
pub(crate) const MAP_ID_PATTERN: &str = r"^[A-Za-z0-9]([-A-Za-z0-9_.]*[A-Za-z0-9])?$";

/// Longest map ID: the label value limit, since the whole ID is the `believer.dev/map` label.
/// [`map_slug`] shortens one that doesn't fit a Fleet name.
pub const MAP_ID_MAX_LEN: usize = 63;

/// Longest map slug: what [`FLEET_NAME_MAX_LEN`] leaves after the longest shard and the version.
pub const MAP_SLUG_MAX_LEN: usize =
    FLEET_NAME_MAX_LEN - (FLEET_SHARD_NAME_MAX_LEN + 1 + SHORT_SHA_LEN + 1);

/// Hex characters of the map ID's SHA-256 that end a shortened slug.
const SLUG_HASH_LEN: usize = 6;

/// Why a name can't be derived.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("shard name {0:?} is longer than {FLEET_SHARD_NAME_MAX_LEN} characters, too long for Fleets")]
    ShardTooLong(String),
    #[error("version {0:?} is not a full 40-character lowercase hex SHA")]
    NotFullSha(String),
    #[error("map ID {0:?} has no characters usable in a name")]
    EmptyMapSlug(String),
    #[error("Fleet name {0:?} is longer than {FLEET_NAME_MAX_LEN} characters")]
    TooLong(String),
    #[error("map IDs {0:?} and {1:?} have the same slug {2:?}, so they would name the same Fleet")]
    SlugCollision(String, String, String),
}

/// The first [`SHORT_SHA_LEN`] characters of a version.
pub fn short_sha(version: &str) -> &str {
    version.get(..SHORT_SHA_LEN).unwrap_or(version)
}

/// The name of the `FleetRelease` (and its `LocalFleetRelease`s) for a build on a shard:
/// `<shard>-<sha8>`, e.g. `test-abc12345`.
///
/// A collision on the 8 characters is unlikely; whoever creates a release checks that an existing
/// one with this name has the same `spec.version` before reusing it.
pub fn release_name(shard: &str, version: &str) -> Result<String, NameError> {
    if shard.len() > FLEET_SHARD_NAME_MAX_LEN {
        return Err(NameError::ShardTooLong(shard.to_string()));
    }
    if !is_full_sha(version) {
        return Err(NameError::NotFullSha(version.to_string()));
    }
    Ok(format!("{shard}-{}", short_sha(version)))
}

/// A map ID in name form: lowercase, with `_` (and any other character a name can't hold) turned
/// into `-`, e.g. `arena-tutorial` for `Arena_Tutorial`.
///
/// A slug longer than [`MAP_SLUG_MAX_LEN`] is cut to its first 19 characters (and any `-` it then
/// ends with), then `-` and the first 6 hex characters of the map ID's SHA-256, e.g.
/// `abcdefghijklmnopqrs-ebf512` for `Abcdefghijklmnopqrstuvwxyz1`. Two long IDs can then share a
/// slug; [`check_map_slugs`] refuses that.
pub fn map_slug(map_id: &str) -> String {
    let slug: String = map_id
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9') => c,
            _ => '-',
        })
        .collect();
    let slug = slug.trim_matches('-');
    if slug.len() <= MAP_SLUG_MAX_LEN {
        return slug.to_string();
    }
    let cut = slug[..MAP_SLUG_MAX_LEN - 1 - SLUG_HASH_LEN].trim_end_matches('-');
    let hash: String = Sha256::digest(map_id.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{cut}-{}", &hash[..SLUG_HASH_LEN])
}

/// The name of the Fleet (and its FleetAutoscaler) for one map of a release:
/// `<shard>-<sha8>-<map slug>`, e.g. `test-abc12345-arena-tutorial`.
pub fn fleet_name(shard: &str, version: &str, map_id: &str) -> Result<String, NameError> {
    let release = release_name(shard, version)?;
    let slug = map_slug(map_id);
    if slug.is_empty() {
        return Err(NameError::EmptyMapSlug(map_id.to_string()));
    }
    let name = format!("{release}-{slug}");
    if name.len() > FLEET_NAME_MAX_LEN {
        return Err(NameError::TooLong(name));
    }
    Ok(name)
}

/// Refuses map IDs that would name the same Fleet: two IDs whose slugs match, such as
/// `QA_BOX_Rotator` and `QA-BOX-Rotator`. `Shard.spec.maps` refuses those in CEL as well, but CEL
/// can't hash, so two long IDs whose shortened slugs match are refused only here.
pub fn check_map_slugs<'a>(map_ids: impl IntoIterator<Item = &'a str>) -> Result<(), NameError> {
    let mut seen = std::collections::BTreeMap::new();
    for map_id in map_ids {
        let slug = map_slug(map_id);
        match seen.insert(slug.clone(), map_id) {
            Some(other) if other != map_id => {
                return Err(NameError::SlugCollision(other.into(), map_id.into(), slug));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The name of the Argo workflow that runs one step of a promotion: `promote-<promotion>-<step>`,
/// e.g. `promote-test-steam-abcde-deploy-backend`. Steps that run in the coordinator have no
/// workflow. Promotions made before steps were split use [`workflow_name`](crate::workflow_name).
pub fn step_workflow_name(promotion_name: &str, step: &str) -> String {
    format!("{WORKFLOW_NAME_PREFIX}{promotion_name}-{step}")
}

/// Whether `map_id` can be used as a map ID: a valid label value, as the schema requires.
pub fn is_valid_map_id(map_id: &str) -> bool {
    let bytes = map_id.as_bytes();
    let alphanumeric = |b: &u8| b.is_ascii_alphanumeric();
    !bytes.is_empty()
        && bytes.len() <= MAP_ID_MAX_LEN
        && bytes.first().is_some_and(alphanumeric)
        && bytes.last().is_some_and(alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The level package of a backend level asset: the asset path without its `.Object` part and any
/// `?…` options, e.g. `/Game/Maps/L_Arena` for `/Game/Maps/L_Arena.L_Arena?MapRowName=Arena`. This
/// is the form the build manifest's cooked map lists are compared in.
pub fn level_package(level_asset: &str) -> &str {
    let path = strip_options(level_asset);
    let name_start = path.rfind('/').map_or(0, |i| i + 1);
    match path[name_start..].find('.') {
        Some(dot) => &path[..name_start + dot],
        None => path,
    }
}

/// The level URL a Fleet's servers start with: `<package>.<object>?MapRowName=<mapId>`.
///
/// Any `?…` the asset carries is replaced, not appended to: two maps may share a level, and
/// `maps.json` then lists the level with one map's row name (e.g.
/// `/Game/Maps/L_Arena.L_Arena?MapRowName=Arena` for both `Arena` and `Arena_Tutorial`). An asset
/// without an object part gets the package's last segment as its object, as Unreal does.
pub fn level_url(level_asset: &str, map_id: &str) -> String {
    let path = strip_options(level_asset);
    let package = level_package(path);
    let object = match path.get(package.len()..) {
        Some(object) if object.len() > 1 => &object[1..],
        _ => package.rsplit('/').next().unwrap_or(package),
    };
    format!("{package}.{object}?MapRowName={map_id}")
}

fn strip_options(level_asset: &str) -> &str {
    let trimmed = level_asset.trim();
    trimmed.split_once('?').map_or(trimmed, |(path, _)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "abc12345e89abcdef0123456789abcdef0123456";

    #[test]
    fn release_names_use_eight_characters() {
        assert_eq!(release_name("test", SHA).unwrap(), "test-abc12345");
        assert_eq!(short_sha(SHA), "abc12345");
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(
            release_name("test", "abc12345"),
            Err(NameError::NotFullSha("abc12345".into()))
        );
        assert_eq!(
            release_name("test", &SHA.to_uppercase()),
            Err(NameError::NotFullSha(SHA.to_uppercase()))
        );
        let long = "a".repeat(FLEET_SHARD_NAME_MAX_LEN + 1);
        assert_eq!(
            release_name(&long, SHA),
            Err(NameError::ShardTooLong(long.clone()))
        );
        assert!(release_name(&long[1..], SHA).is_ok());
    }

    #[test]
    fn map_slugs_are_name_safe() {
        assert_eq!(map_slug("Arena_Tutorial"), "arena-tutorial");
        assert_eq!(map_slug("Tutorials"), "tutorials");
        assert_eq!(map_slug("Arena.v2"), "arena-v2");
        assert_eq!(map_slug("_Edge_"), "edge");
        assert_eq!(map_slug("__"), "");
    }

    #[test]
    fn fleet_names_join_release_and_slug() {
        assert_eq!(
            fleet_name("test", SHA, "Arena_Tutorial").unwrap(),
            "test-abc12345-arena-tutorial"
        );
        assert_eq!(
            fleet_name("test", SHA, "__"),
            Err(NameError::EmptyMapSlug("__".into()))
        );

        // The longest shard leaves MAP_SLUG_MAX_LEN characters for the slug, and every map ID
        // fits: a longer slug is shortened.
        assert_eq!(FLEET_NAME_MAX_LEN, 51);
        assert_eq!(MAP_SLUG_MAX_LEN, 26);
        let shard = "s".repeat(FLEET_SHARD_NAME_MAX_LEN);
        let fits = "m".repeat(MAP_SLUG_MAX_LEN);
        assert_eq!(
            fleet_name(&shard, SHA, &fits).unwrap().len(),
            FLEET_NAME_MAX_LEN
        );
        let longest = "m".repeat(MAP_ID_MAX_LEN);
        assert!(is_valid_map_id(&longest));
        assert_eq!(
            fleet_name(&shard, SHA, &longest).unwrap().len(),
            FLEET_NAME_MAX_LEN
        );
    }

    #[test]
    fn slugs_that_fit_are_not_shortened() {
        // Fleet names of maps that fit don't change when longer IDs are shortened.
        for (map_id, slug) in [
            ("Arena_Tutorial", "arena-tutorial"),
            ("QA_BOX_Rotator", "qa-box-rotator"),
            ("CopperLanternHall", "copperlanternhall"),
            ("QA_BOX_Waterfalls", "qa-box-waterfalls"),
            ("Lab_ClockworkShutdowns", "lab-clockworkshutdowns"),
            ("Abcdefghijklmnopqrstuvwxyz", "abcdefghijklmnopqrstuvwxyz"),
        ] {
            assert_eq!(map_slug(map_id), slug);
        }
    }

    #[test]
    fn long_slugs_are_cut_and_hashed() {
        // 27 characters: the first 19, `-`, and the SHA-256 of the ID (`printf %s ID | sha256sum`).
        assert_eq!(
            map_slug("Abcdefghijklmnopqrstuvwxyz1"),
            "abcdefghijklmnopqrs-ebf512"
        );
        // A cut that ends in `-` drops it.
        assert_eq!(
            map_slug("Abcdefghijklmnopqr_Stuvwxyz12"),
            "abcdefghijklmnopqr-765839"
        );
        // Long IDs that share their first 19 characters still get their own slugs.
        let a = format!("{}_A", "m".repeat(30));
        let b = format!("{}_B", "m".repeat(30));
        assert_ne!(map_slug(&a), map_slug(&b));
        check_map_slugs([a.as_str(), b.as_str()]).unwrap();
        for map_id in [a.as_str(), &"m".repeat(MAP_ID_MAX_LEN)] {
            let slug = map_slug(map_id);
            assert_eq!(slug.len(), MAP_SLUG_MAX_LEN, "{slug}");
            assert!(is_valid_map_id(&slug), "{slug}");
        }
    }

    #[test]
    fn fleet_names_leave_room_for_two_agones_suffixes() {
        // <fleet>-xxxxx names the GameServerSet and <set>-xxxxx the GameServer, both under 63 uncut.
        let fleet = "f".repeat(FLEET_NAME_MAX_LEN);
        let game_server = format!("{fleet}-abcde-fghij");
        assert_eq!(game_server.len(), NAME_MAX_LEN);
    }

    #[test]
    fn long_map_ids_fit_the_longest_shard() {
        let shard = "s".repeat(FLEET_SHARD_NAME_MAX_LEN);
        for map_id in [
            "CopperLanternHall",
            "QA_BOX_Waterfalls",
            "Lab_ClockworkShutdowns",
        ] {
            assert!(is_valid_map_id(map_id), "{map_id}");
            let name = fleet_name(&shard, SHA, map_id).unwrap();
            assert!(name.len() <= FLEET_NAME_MAX_LEN, "{name}");
        }
    }

    #[test]
    fn map_slug_collisions_are_refused() {
        check_map_slugs(["Arena", "Arena_Tutorial", "QA_BOX_Rotator"]).unwrap();
        // The same ID twice names one Fleet, not two.
        check_map_slugs(["Arena", "Arena"]).unwrap();
        assert_eq!(
            check_map_slugs(["QA_BOX_Rotator", "Arena", "QA-BOX-Rotator"]),
            Err(NameError::SlugCollision(
                "QA_BOX_Rotator".into(),
                "QA-BOX-Rotator".into(),
                "qa-box-rotator".into()
            ))
        );
        assert!(check_map_slugs(["qa.box.rotator", "QA_BOX_Rotator"]).is_err());
    }

    #[test]
    fn step_workflow_names() {
        assert_eq!(
            step_workflow_name("test-steam-abcde", "deploy-backend"),
            "promote-test-steam-abcde-deploy-backend"
        );
    }

    #[test]
    fn map_ids_are_label_values() {
        assert!(is_valid_map_id("Arena_Tutorial"));
        assert!(is_valid_map_id("a"));
        assert!(!is_valid_map_id(""));
        assert!(!is_valid_map_id("_Leading"));
        assert!(!is_valid_map_id("Trailing-"));
        assert!(!is_valid_map_id("has space"));
        assert!(is_valid_map_id(&"m".repeat(MAP_ID_MAX_LEN)));
        assert!(!is_valid_map_id(&"m".repeat(MAP_ID_MAX_LEN + 1)));
    }

    #[test]
    fn level_packages_drop_object_and_options() {
        assert_eq!(
            level_package("/Game/Maps/L_Arena.L_Arena?MapRowName=Arena"),
            "/Game/Maps/L_Arena"
        );
        assert_eq!(
            level_package("/Game/Maps/L_Arena.L_Arena"),
            "/Game/Maps/L_Arena"
        );
        assert_eq!(level_package("/Game/Maps/L_Arena"), "/Game/Maps/L_Arena");
        assert_eq!(
            level_package("/Plugin.Name/Maps/L_Arena?x=1"),
            "/Plugin.Name/Maps/L_Arena"
        );
    }

    #[test]
    fn level_urls_carry_exactly_one_row_name() {
        // An asset without options.
        assert_eq!(
            level_url("/Game/Maps/L_Arena.L_Arena", "Arena"),
            "/Game/Maps/L_Arena.L_Arena?MapRowName=Arena"
        );
        // Two maps sharing a level: the `maps.json` suffix names one of them; each gets its own.
        let shared = "/Game/Maps/L_Arena.L_Arena?MapRowName=Arena";
        assert_eq!(level_url(shared, "Arena"), shared);
        assert_eq!(
            level_url(shared, "Arena_Tutorial"),
            "/Game/Maps/L_Arena.L_Arena?MapRowName=Arena_Tutorial"
        );
        // Other options are replaced too, and stray whitespace dropped.
        assert_eq!(
            level_url(
                " /Game/Maps/L_Arena.L_Arena?Game=X&MapRowName=Arena ",
                "Arena"
            ),
            "/Game/Maps/L_Arena.L_Arena?MapRowName=Arena"
        );
        // No object part: the package name is the object.
        assert_eq!(
            level_url("/Game/Maps/L_Arena", "Arena"),
            "/Game/Maps/L_Arena.L_Arena?MapRowName=Arena"
        );
        assert_eq!(
            level_url("/Game/Maps/L_Arena.", "Arena"),
            "/Game/Maps/L_Arena.L_Arena?MapRowName=Arena"
        );
    }
}
