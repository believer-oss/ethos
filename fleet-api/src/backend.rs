//! Backend versions, as reported by a shard's healthcheck.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One backend part's version string, `<build time>_<release>_<branch>_<sha>`,
/// e.g. `2026-01-02-03-04-05_1.2.3_main_abc1234`.
///
/// The build time and release never contain `_`; the SHA is the last segment. Everything between
/// the release and the SHA is the branch, so a branch may itself contain `_`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BackendVersion {
    /// When the part's artifact was built, in UTC, as written in the version string
    /// (e.g. `2026-01-02-03-04-05`). Not a deploy time: deploying leaves it unchanged, and nothing
    /// reports when a part was deployed.
    pub built_at: String,
    /// The backend platform release (e.g. `1.2.3`).
    pub release: String,
    pub branch: String,
    /// The commit, usually abbreviated.
    pub sha: String,
}

impl FromStr for BackendVersion {
    type Err = ParseBackendVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let malformed = || ParseBackendVersionError(s.to_string());
        let (built_at, rest) = s.split_once('_').ok_or_else(malformed)?;
        let (release, rest) = rest.split_once('_').ok_or_else(malformed)?;
        let (branch, sha) = rest.rsplit_once('_').ok_or_else(malformed)?;
        if [built_at, release, branch, sha]
            .iter()
            .any(|p| p.is_empty())
        {
            return Err(malformed());
        }
        Ok(BackendVersion {
            built_at: built_at.to_string(),
            release: release.to_string(),
            branch: branch.to_string(),
            sha: sha.to_string(),
        })
    }
}

impl fmt::Display for BackendVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}_{}_{}_{}",
            self.built_at, self.release, self.branch, self.sha
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("backend version {0:?} is not <build time>_<release>_<branch>_<sha>")]
pub struct ParseBackendVersionError(pub String);

/// The version of each backend part, from the healthcheck JSON field named in brackets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackendParts {
    /// `platformVersion`
    #[serde(default)]
    pub platform: Option<String>,
    /// `portalVersion`
    #[serde(default)]
    pub portal: Option<String>,
    /// `contentVersion`
    #[serde(default)]
    pub content: Option<String>,
    /// `configVersion`
    #[serde(default)]
    pub config: Option<String>,
}

impl BackendParts {
    /// The parts that are set, by name, in a fixed order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("platform", &self.platform),
            ("portal", &self.portal),
            ("content", &self.content),
            ("config", &self.config),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.as_deref().map(|v| (name, v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_four_segments() {
        let v: BackendVersion = "2026-01-02-03-04-05_1.2.3_main_abc1234".parse().unwrap();
        assert_eq!(v.built_at, "2026-01-02-03-04-05");
        assert_eq!(v.release, "1.2.3");
        assert_eq!(v.branch, "main");
        assert_eq!(v.sha, "abc1234");
        assert_eq!(v.to_string(), "2026-01-02-03-04-05_1.2.3_main_abc1234");
    }

    #[test]
    fn branch_may_contain_underscores() {
        let v: BackendVersion = "2026-01-02-03-04-05_1.2.3_release_1_2_abc1234"
            .parse()
            .unwrap();
        assert_eq!(v.branch, "release_1_2");
        assert_eq!(v.sha, "abc1234");
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "abc1234",
            "2026-01-02_1.2.3_abc1234",
            "2026-01-02__main_abc1234",
            "2026-01-02_1.2.3_main_",
        ] {
            assert!(bad.parse::<BackendVersion>().is_err(), "{bad:?}");
        }
    }

    fn parts(values: [Option<&str>; 4]) -> BackendParts {
        let [platform, portal, content, config] = values.map(|v| v.map(str::to_string));
        BackendParts {
            platform,
            portal,
            content,
            config,
        }
    }

    #[test]
    fn iter_skips_unset_parts() {
        let p = parts([Some("p"), None, Some("c"), None]);
        assert_eq!(
            p.iter().collect::<Vec<_>>(),
            [("platform", "p"), ("content", "c")]
        );
    }
}
