use crate::BIN_SUFFIX;
use anyhow::anyhow;
use octocrab::models::repos::Release;
use octocrab::Octocrab;

pub async fn get_latest_github_release(
    octocrab: &Octocrab,
    app_name: &str,
    repo_owner: &str,
    repo_name: &str,
) -> anyhow::Result<Release> {
    let releases = octocrab
        .repos(repo_owner, repo_name)
        .releases()
        .list()
        .per_page(100)
        .send()
        .await?;

    let mut latest = releases
        .into_iter()
        .filter_map(|release| {
            if release.draft || release.prerelease {
                return None;
            }

            // if release doesn't match the format app_name-vX.Y.Z, skip it
            if !release.tag_name.starts_with(&format!("{}-v", app_name)) {
                return None;
            }

            // get semver
            let version = release
                .tag_name
                .strip_prefix(&format!("{}-v", app_name))
                .unwrap();
            if semver::Version::parse(version).is_err() {
                return None;
            }

            if release
                .assets
                .iter()
                .any(|asset| asset.name == format!("{}{}", &app_name, BIN_SUFFIX))
            {
                return Some(release);
            }

            None
        })
        .collect::<Vec<Release>>();

    // sort by semver
    latest.sort_by(|a, b| {
        // we can unwrap because we asserted this format earlier
        let a = a
            .tag_name
            .strip_prefix(&format!("{}-v", &app_name))
            .unwrap();
        let b = b
            .tag_name
            .strip_prefix(&format!("{}-v", &app_name))
            .unwrap();
        let a = semver::Version::parse(a).unwrap();
        let b = semver::Version::parse(b).unwrap();

        // reverse it
        b.cmp(&a)
    });

    match latest.first() {
        Some(latest) => Ok(latest.clone()),
        None => Err(anyhow!("No latest version found")),
    }
}
