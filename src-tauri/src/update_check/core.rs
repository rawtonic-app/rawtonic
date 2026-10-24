// SPDX-License-Identifier: AGPL-3.0-or-later
use semver::Version;
use serde::{Deserialize, Serialize};

const INTERVAL: u64 = 24 * 60 * 60;

/// The action is never called when automatic checks are disabled or not due.
pub fn when_due<T>(
    enabled: bool,
    last: Option<u64>,
    now: u64,
    manual: bool,
    action: impl FnOnce() -> T,
) -> Option<T> {
    (manual || (enabled && last.is_none_or(|last| last > now || now - last >= INTERVAL)))
        .then(action)
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub release_url: String,
    pub app_image: bool,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
}

pub fn parse_version(tag: &str) -> Result<Version, semver::Error> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag))
}

pub fn include_prereleases(current: &str, chosen: Option<bool>) -> Result<bool, String> {
    Ok(chosen.unwrap_or(
        !parse_version(current)
            .map_err(|e| e.to_string())?
            .pre
            .is_empty(),
    ))
}

pub fn select_release(
    json: &[u8],
    current: &str,
    prereleases: bool,
    app_image: bool,
) -> Result<Option<UpdateInfo>, String> {
    let current = parse_version(current).map_err(|e| e.to_string())?;
    let releases: Vec<Release> = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let version = parse_version(&r.tag_name).ok()?;
            if (!prereleases && (r.prerelease || !version.pre.is_empty()))
                || version.cmp_precedence(&current).is_le()
            {
                return None;
            }
            // Only GitHub release pages from this repository can reach the opener.
            if !r
                .html_url
                .starts_with("https://github.com/rawtonic-app/rawtonic/releases/tag/")
            {
                return None;
            }
            Some((version, r.html_url))
        })
        .max_by(|a, b| a.0.cmp_precedence(&b.0))
        .map(|(version, release_url)| UpdateInfo {
            version: version.to_string(),
            release_url,
            app_image,
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interval_and_opt_out_do_not_invoke_the_request_seam() {
        for (enabled, last, now, manual, expected) in [
            (true, None, 0, false, true),
            (false, None, 100000, false, false),
            (true, Some(10), 86409, false, false),
            (true, Some(10), 86410, false, true),
            (true, Some(100000), 1, false, true),
            (false, Some(10), 11, true, true),
        ] {
            let mut requests = 0;
            when_due(enabled, last, now, manual, || requests += 1);
            assert_eq!(requests == 1, expected);
        }
    }
    #[test]
    fn channel_defaults_follow_the_running_version_with_explicit_overrides() {
        assert!(include_prereleases("0.1.0-alpha.3", None).unwrap());
        assert!(!include_prereleases("0.1.0", None).unwrap());
        assert!(!include_prereleases("0.1.0-alpha.3", Some(false)).unwrap());
        assert!(include_prereleases("0.1.0", Some(true)).unwrap());
    }
    #[test]
    fn semver_prerelease_order_and_build_metadata() {
        let tags = [
            "v0.1.0-alpha.3",
            "0.1.0-alpha.4",
            "0.1.0-alpha.10",
            "0.1.0-beta.1",
            "0.1.0",
        ];
        for pair in tags.windows(2) {
            assert!(
                parse_version(pair[0])
                    .unwrap()
                    .cmp_precedence(&parse_version(pair[1]).unwrap())
                    .is_lt()
            );
        }
        assert!(select_release(br#"[{"tag_name":"v0.1.0+build.2","html_url":"https://github.com/rawtonic-app/rawtonic/releases/tag/v0.1.0","draft":false,"prerelease":false}]"#, "0.1.0+build.1", true, false).unwrap().is_none());
    }
    #[test]
    fn synthetic_api_fixture_skips_drafts_invalid_tags_and_sorts_by_version() {
        let json = include_bytes!("releases.fixture.json");
        assert_eq!(
            select_release(json, "0.1.0-alpha.3", true, false)
                .unwrap()
                .unwrap()
                .version,
            "0.2.0-alpha.4"
        );
        assert_eq!(
            select_release(json, "0.1.0-alpha.3", false, false)
                .unwrap()
                .unwrap()
                .version,
            "0.1.0"
        );
        assert!(
            select_release(json, "0.2.0", true, false)
                .unwrap()
                .is_none()
        );
        assert!(select_release(b"garbage", "0.1.0", true, false).is_err());
        assert!(select_release(json, "bad", true, false).is_err());
    }
}
