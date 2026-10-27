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
    /// The installer for this computer, when the release has one; the notice
    /// falls back to the release page otherwise.
    pub download_url: Option<String>,
    pub asset_name: Option<String>,
    pub asset_size: Option<u64>,
    /// The release notes (Markdown) from the same response.
    pub notes: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arch {
    Arm64,
    X64,
}

/// How Rawtonic came onto a Linux system, which decides the package it is offered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinuxPackage {
    AppImage,
    Deb,
    Rpm,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
    pub linux: LinuxPackage,
}

/// A running AppImage gets AppImages; otherwise the distribution's own package
/// format (from `/etc/os-release`) comes first, then the AppImage.
pub fn linux_package(app_image: bool, os_release: Option<&str>) -> LinuxPackage {
    if app_image {
        return LinuxPackage::AppImage;
    }
    let ids = os_release
        .into_iter()
        .flat_map(str::lines)
        .filter_map(|line| {
            line.strip_prefix("ID=")
                .or_else(|| line.strip_prefix("ID_LIKE="))
        })
        .flat_map(|v| v.trim_matches(|c| c == '"' || c == '\'').split_whitespace())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let has = |names: &[&str]| ids.iter().any(|id| names.contains(&id.as_str()));
    if has(&["debian", "ubuntu"]) {
        LinuxPackage::Deb
    } else if has(&["fedora", "rhel", "centos", "suse", "opensuse"]) {
        LinuxPackage::Rpm
    } else {
        LinuxPackage::AppImage
    }
}

const DOWNLOAD_PREFIX: &str = "https://github.com/rawtonic-app/rawtonic/releases/download/";
/// Release notes beyond this are cut; the release page has them whole.
const NOTES_LIMIT: usize = 64 * 1024;

/// The installer for `platform` among a release's files, best first by the
/// names the packaging writes (`Rawtonic_<version>_aarch64.dmg`,
/// `…_x86_64.dmg`, `…_amd64.AppImage`; Tauri's `…_amd64.deb`,
/// `…-1.x86_64.rpm`, `…_x64-setup.exe`, `…_x64_en-US.msi`). Checksums, update
/// files (`.zsync`, `.sig`, `.tar.gz`) and source tarballs never match.
pub fn pick_asset(assets: &[Asset], platform: Platform) -> Option<&Asset> {
    let kinds: &[&str] = match platform.os {
        Os::Mac => &[".dmg"],
        Os::Windows => &["-setup.exe", ".msi"],
        Os::Linux => match platform.linux {
            LinuxPackage::AppImage => &[".appimage", ".deb", ".rpm"],
            LinuxPackage::Deb => &[".deb", ".appimage"],
            LinuxPackage::Rpm => &[".rpm", ".appimage"],
        },
    };
    let arch = match platform.arch {
        Arch::Arm64 => "arm64",
        Arch::X64 => "x64",
    };
    let usable = assets
        .iter()
        .filter(|a| a.browser_download_url.starts_with(DOWNLOAD_PREFIX))
        .map(|a| (a, a.name.to_ascii_lowercase()));
    let tokens = |name: &str| -> Vec<String> {
        name.replace("x86_64", "x64")
            .replace("amd64", "x64")
            .replace("aarch64", "arm64")
            .split(['_', '-', '.'])
            .map(str::to_owned)
            .collect()
    };
    // Every kind for this architecture before a universal build of any kind.
    [arch, "universal"]
        .into_iter()
        .flat_map(|want| kinds.iter().map(move |kind| (*kind, want)))
        .find_map(|(kind, want)| {
            usable
                .clone()
                .find(|(_, name)| name.ends_with(kind) && tokens(name).iter().any(|t| t == want))
                .map(|(a, _)| a)
        })
}

fn cut(mut notes: String) -> String {
    if notes.len() > NOTES_LIMIT {
        let mut end = NOTES_LIMIT;
        while !notes.is_char_boundary(end) {
            end -= 1;
        }
        notes.truncate(end);
    }
    notes
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
    platform: Platform,
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
            Some((version, r))
        })
        .max_by(|a, b| a.0.cmp_precedence(&b.0))
        .map(|(version, r)| {
            let asset = pick_asset(&r.assets, platform);
            UpdateInfo {
                version: version.to_string(),
                download_url: asset.map(|a| a.browser_download_url.clone()),
                asset_name: asset.map(|a| a.name.clone()),
                asset_size: asset.map(|a| a.size).filter(|size| *size > 0),
                release_url: r.html_url,
                app_image,
                notes: cut(r.body.unwrap_or_default()),
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    const MAC_ARM: Platform = Platform {
        os: Os::Mac,
        arch: Arch::Arm64,
        linux: LinuxPackage::AppImage,
    };
    fn assets(tag: &str, names: &[&str]) -> Vec<Asset> {
        names
            .iter()
            .map(|name| Asset {
                name: (*name).to_owned(),
                browser_download_url: format!("{DOWNLOAD_PREFIX}{tag}/{name}"),
                size: 0,
            })
            .collect()
    }
    fn platform(os: Os, arch: Arch, linux: LinuxPackage) -> Platform {
        Platform { os, arch, linux }
    }

    #[test]
    fn picks_the_installer_from_real_release_files() {
        use {Arch::*, LinuxPackage::*, Os::*};
        // The files of published releases, as `gh release view` lists them.
        let alpha_7_1 = assets(
            "v0.1.0-alpha.7.1",
            &[
                "libgphoto2-2.5.34.tar.bz2",
                "libtool-2.6.2.tar.xz",
                "libusb-1.0.30.tar.bz2",
                "Rawtonic_0.1.0-alpha.7.1_aarch64.dmg",
                "Rawtonic_0.1.0-alpha.7.1_amd64.AppImage",
                "Rawtonic_0.1.0-alpha.7.1_amd64.AppImage.zsync",
                "Rawtonic_0.1.0-alpha.7.1_x86_64.dmg",
                "SHA256SUMS.txt",
            ],
        );
        let alpha_3 = assets(
            "v0.1.0-alpha.3",
            &[
                "libgphoto2-2.5.34.tar.bz2",
                "Rawtonic_0.1.0-alpha.3_aarch64.dmg",
                "Rawtonic_0.1.0-alpha.3_amd64.AppImage",
                "Rawtonic_0.1.0-alpha.3_amd64.AppImage.zsync",
            ],
        );
        let alpha_1 = assets("v0.1.0-alpha.1", &["Rawtonic_0.1.0-alpha.1_aarch64.dmg"]);
        // Every file Tauri's bundlers name, for platforms not released yet.
        let full = assets(
            "v1.0.0",
            &[
                "Rawtonic_1.0.0_aarch64.dmg",
                "Rawtonic_1.0.0_x86_64.dmg",
                "Rawtonic_1.0.0_universal.dmg",
                "Rawtonic_aarch64.app.tar.gz",
                "Rawtonic_aarch64.app.tar.gz.sig",
                "Rawtonic_1.0.0_amd64.AppImage",
                "Rawtonic_1.0.0_aarch64.AppImage",
                "Rawtonic_1.0.0_amd64.deb",
                "Rawtonic_1.0.0_arm64.deb",
                "Rawtonic-1.0.0-1.x86_64.rpm",
                "Rawtonic-1.0.0-1.aarch64.rpm",
                "Rawtonic_1.0.0_x64_en-US.msi",
                "Rawtonic_1.0.0_x64-setup.exe",
                "Rawtonic_1.0.0_arm64-setup.exe",
            ],
        );
        let universal_only = assets("v1.0.0", &["Rawtonic_1.0.0_universal.dmg"]);
        let msi_only = assets("v1.0.0", &["Rawtonic_1.0.0_x64_en-US.msi"]);
        for (files, os, arch, linux, expected) in [
            (
                &alpha_7_1,
                Mac,
                Arm64,
                AppImage,
                Some("Rawtonic_0.1.0-alpha.7.1_aarch64.dmg"),
            ),
            (
                &alpha_7_1,
                Mac,
                X64,
                AppImage,
                Some("Rawtonic_0.1.0-alpha.7.1_x86_64.dmg"),
            ),
            (
                &alpha_7_1,
                Linux,
                X64,
                AppImage,
                Some("Rawtonic_0.1.0-alpha.7.1_amd64.AppImage"),
            ),
            // No deb was released: a Debian system gets the AppImage.
            (
                &alpha_7_1,
                Linux,
                X64,
                Deb,
                Some("Rawtonic_0.1.0-alpha.7.1_amd64.AppImage"),
            ),
            (&alpha_7_1, Linux, Arm64, AppImage, None),
            (&alpha_7_1, Windows, X64, AppImage, None),
            (&alpha_3, Mac, X64, AppImage, None),
            (
                &alpha_3,
                Linux,
                X64,
                Rpm,
                Some("Rawtonic_0.1.0-alpha.3_amd64.AppImage"),
            ),
            (
                &alpha_1,
                Mac,
                Arm64,
                AppImage,
                Some("Rawtonic_0.1.0-alpha.1_aarch64.dmg"),
            ),
            (&alpha_1, Linux, X64, AppImage, None),
            (
                &full,
                Mac,
                Arm64,
                AppImage,
                Some("Rawtonic_1.0.0_aarch64.dmg"),
            ),
            (&full, Mac, X64, AppImage, Some("Rawtonic_1.0.0_x86_64.dmg")),
            (
                &full,
                Linux,
                X64,
                AppImage,
                Some("Rawtonic_1.0.0_amd64.AppImage"),
            ),
            (
                &full,
                Linux,
                Arm64,
                AppImage,
                Some("Rawtonic_1.0.0_aarch64.AppImage"),
            ),
            (&full, Linux, X64, Deb, Some("Rawtonic_1.0.0_amd64.deb")),
            (&full, Linux, Arm64, Deb, Some("Rawtonic_1.0.0_arm64.deb")),
            (&full, Linux, X64, Rpm, Some("Rawtonic-1.0.0-1.x86_64.rpm")),
            (
                &full,
                Linux,
                Arm64,
                Rpm,
                Some("Rawtonic-1.0.0-1.aarch64.rpm"),
            ),
            (
                &full,
                Windows,
                X64,
                AppImage,
                Some("Rawtonic_1.0.0_x64-setup.exe"),
            ),
            (
                &full,
                Windows,
                Arm64,
                AppImage,
                Some("Rawtonic_1.0.0_arm64-setup.exe"),
            ),
            (
                &universal_only,
                Mac,
                X64,
                AppImage,
                Some("Rawtonic_1.0.0_universal.dmg"),
            ),
            (
                &msi_only,
                Windows,
                X64,
                AppImage,
                Some("Rawtonic_1.0.0_x64_en-US.msi"),
            ),
            (&msi_only, Windows, Arm64, AppImage, None),
        ] {
            assert_eq!(
                pick_asset(files, platform(os, arch, linux)).map(|a| a.name.as_str()),
                expected,
                "{os:?} {arch:?} {linux:?}"
            );
        }
    }

    #[test]
    fn only_this_repositorys_download_urls_are_offered() {
        let foreign = vec![Asset {
            name: "Rawtonic_1.0.0_aarch64.dmg".into(),
            browser_download_url: "https://example.invalid/Rawtonic_1.0.0_aarch64.dmg".into(),
            size: 1,
        }];
        assert_eq!(pick_asset(&foreign, MAC_ARM), None);
    }

    #[test]
    fn linux_package_follows_the_appimage_and_os_release() {
        use LinuxPackage::*;
        for (app_image, os_release, expected) in [
            (true, Some("ID=ubuntu\nID_LIKE=debian\n"), AppImage),
            (
                false,
                Some("NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n"),
                Deb,
            ),
            (false, Some("ID=debian\n"), Deb),
            (
                false,
                Some("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"),
                Deb,
            ),
            (false, Some("ID=fedora\n"), Rpm),
            (
                false,
                Some("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n"),
                Rpm,
            ),
            (
                false,
                Some("ID=rocky\nID_LIKE=\"rhel centos fedora\"\n"),
                Rpm,
            ),
            (false, Some("ID=arch\n"), AppImage),
            (false, None, AppImage),
        ] {
            assert_eq!(
                linux_package(app_image, os_release),
                expected,
                "{os_release:?}"
            );
        }
    }

    #[test]
    fn the_newest_release_brings_its_installer_and_notes() {
        let json = br###"[{"tag_name":"v0.1.0-alpha.8","html_url":"https://github.com/rawtonic-app/rawtonic/releases/tag/v0.1.0-alpha.8","draft":false,"prerelease":true,
            "body":"## Fixed\n- one","assets":[{"name":"Rawtonic_0.1.0-alpha.8_aarch64.dmg","size":412000000,"browser_download_url":"https://github.com/rawtonic-app/rawtonic/releases/download/v0.1.0-alpha.8/Rawtonic_0.1.0-alpha.8_aarch64.dmg"}]}]"###;
        let update = select_release(json, "0.1.0-alpha.7.1", true, MAC_ARM, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            update.asset_name.as_deref(),
            Some("Rawtonic_0.1.0-alpha.8_aarch64.dmg")
        );
        assert_eq!(
            update.download_url.as_deref(),
            Some(
                "https://github.com/rawtonic-app/rawtonic/releases/download/v0.1.0-alpha.8/Rawtonic_0.1.0-alpha.8_aarch64.dmg"
            )
        );
        assert_eq!(update.asset_size, Some(412_000_000));
        assert_eq!(update.notes, "## Fixed\n- one");
        let intel = Platform {
            arch: Arch::X64,
            ..MAC_ARM
        };
        let update = select_release(json, "0.1.0-alpha.7.1", true, intel, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            (update.download_url, update.asset_name, update.asset_size),
            (None, None, None)
        );
        assert_eq!(cut("é".repeat(NOTES_LIMIT)).len(), NOTES_LIMIT);
    }
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
        assert!(select_release(br#"[{"tag_name":"v0.1.0+build.2","html_url":"https://github.com/rawtonic-app/rawtonic/releases/tag/v0.1.0","draft":false,"prerelease":false}]"#, "0.1.0+build.1", true, MAC_ARM, false).unwrap().is_none());
    }
    #[test]
    fn synthetic_api_fixture_skips_drafts_invalid_tags_and_sorts_by_version() {
        let json = include_bytes!("releases.fixture.json");
        assert_eq!(
            select_release(json, "0.1.0-alpha.3", true, MAC_ARM, false)
                .unwrap()
                .unwrap()
                .version,
            "0.2.0-alpha.4"
        );
        assert_eq!(
            select_release(json, "0.1.0-alpha.3", false, MAC_ARM, false)
                .unwrap()
                .unwrap()
                .version,
            "0.1.0"
        );
        assert!(
            select_release(json, "0.2.0", true, MAC_ARM, false)
                .unwrap()
                .is_none()
        );
        assert!(select_release(b"garbage", "0.1.0", true, MAC_ARM, false).is_err());
        assert!(select_release(json, "bad", true, MAC_ARM, false).is_err());
    }
}
