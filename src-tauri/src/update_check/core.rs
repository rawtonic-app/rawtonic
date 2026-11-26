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
    /// "Install update" is offered: the release carries a signed update
    /// manifest and this copy can replace itself (user decision 2026-11-18).
    pub in_app: bool,
    /// Why the notice links to the browser instead, when `in_app` is false.
    pub fallback: Option<Fallback>,
    /// What "Install update" fetches after the click; never sent to the webview.
    #[serde(skip)]
    pub offer: Option<Offer>,
}

/// Where the in-app update of one release comes from: that release's own
/// `latest.json` and this copy's package key in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub version: String,
    pub manifest_url: String,
    pub target: &'static str,
    /// The prefix every package URL of this release starts with.
    pub package_prefix: String,
}

/// Why a release is offered through the browser instead of "Install update".
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Fallback {
    /// The release has no `latest.json` (releases before in-app updates).
    NoManifest,
    /// The release has no installer for this computer (its architecture is
    /// not built), so it has no update package for it either.
    NoPackage,
    /// Windows, installed for all users in Program Files (the MSI): replacing
    /// it needs administrator approval, which the installer asks for.
    AllUsers,
    /// macOS, run from the disk image or not moved out of the Downloads
    /// quarantine (read-only): it must be moved to Applications first.
    MoveToApplications,
    /// The Windows portable zip: no installer manages it.
    Portable,
    /// A Linux distribution package (or anything but the AppImage): the
    /// package manager owns its files.
    Package,
    /// A development build outside an app bundle.
    NotInstalled,
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

/// How this copy of Rawtonic came onto the computer, which decides whether it
/// can update itself and which download the browser fallback offers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Install {
    /// A macOS app bundle on a writable volume (Applications, any folder).
    MacApp,
    /// A macOS app bundle on a read-only volume: the mounted disk image, or
    /// App Translocation of an app still in its download folder.
    MacReadOnly,
    /// A running AppImage (`APPIMAGE` set).
    AppImage,
    /// Linux without an AppImage: a distribution package or a build folder.
    LinuxOther,
    /// Windows, the per-user setup (`uninstall.exe` beside the program,
    /// outside Program Files).
    WindowsSetup,
    /// Windows, installed for all users under Program Files (the MSI).
    WindowsAllUsers,
    /// Windows without an installer: the portable zip, or a build folder.
    WindowsPortable,
    /// A program outside an app bundle on macOS (a development build).
    Unbundled,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
    pub linux: LinuxPackage,
    pub install: Install,
}

/// macOS: `exe` inside `<name>.app/Contents/MacOS/` is a bundle; `read_only`
/// is whether the bundle's volume is mounted read-only.
pub fn mac_install(
    exe: &std::path::Path,
    read_only: impl FnOnce(&std::path::Path) -> bool,
) -> Install {
    let bundle = exe
        .parent()
        .filter(|d| d.file_name().is_some_and(|n| n == "MacOS"))
        .and_then(std::path::Path::parent)
        .filter(|d| d.file_name().is_some_and(|n| n == "Contents"))
        .and_then(std::path::Path::parent)
        .filter(|d| d.extension().is_some_and(|e| e == "app"));
    match bundle {
        None => Install::Unbundled,
        Some(bundle) if read_only(bundle) => Install::MacReadOnly,
        Some(_) => Install::MacApp,
    }
}

/// Linux: the AppImage file this process runs from, or None. `APPIMAGE` is
/// trusted only when the program is Tauri's AppImage build (`bundle_is_appimage`)
/// and runs from inside the AppImage's own mount (`APPDIR`): a .deb or .rpm
/// copy started from another AppImage's terminal inherits that AppImage's
/// variables and must never replace its file.
// Called only on Unix (`started_appimage`); tested everywhere.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn running_appimage(
    bundle_is_appimage: bool,
    appimage: Option<&std::path::Path>,
    appdir: Option<&std::path::Path>,
    exe: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let (appimage, appdir) = (appimage?, appdir?);
    (bundle_is_appimage
        && appdir.is_absolute()
        && appimage.is_absolute()
        && exe.starts_with(appdir))
    .then(|| appimage.to_path_buf())
}

/// Windows: the folder of the program, whether the setup's `uninstall.exe`
/// lies beside it, and the Program Files folders (`%ProgramFiles%`,
/// `%ProgramW6432%`, `%ProgramFiles(x86)%`). Paths compare without case and
/// with either separator.
pub fn windows_install(exe_dir: &str, has_uninstaller: bool, program_files: &[String]) -> Install {
    let norm = |p: &str| {
        // Tauri canonicalizes the program's path, which on Windows yields the
        // verbatim form `\\?\C:\Program Files\…`: without the prefix such a
        // copy would not be seen under Program Files.
        let p = p.strip_prefix(r"\\?\").unwrap_or(p);
        let mut p = p.replace('/', "\\").to_lowercase();
        while p.ends_with('\\') {
            p.pop();
        }
        p
    };
    let dir = norm(exe_dir);
    let under = program_files.iter().map(|p| norm(p)).any(|root| {
        !root.is_empty()
            && dir
                .strip_prefix(&root)
                .is_some_and(|rest| rest.starts_with('\\'))
    });
    if under {
        Install::WindowsAllUsers
    } else if has_uninstaller {
        Install::WindowsSetup
    } else {
        Install::WindowsPortable
    }
}

/// The package key of this copy in a release's `latest.json` (Tauri's
/// `{os}-{arch}-{installer}`), or why it updates through the browser.
pub fn updater_target(platform: Platform) -> Result<&'static str, Fallback> {
    use {Arch::*, Install::*};
    match (platform.install, platform.arch) {
        (MacApp, Arm64) => Ok("darwin-aarch64-app"),
        (MacApp, X64) => Ok("darwin-x86_64-app"),
        (AppImage, X64) => Ok("linux-x86_64-appimage"),
        (AppImage, Arm64) => Ok("linux-aarch64-appimage"),
        (WindowsSetup, X64) => Ok("windows-x86_64-nsis"),
        (WindowsSetup, Arm64) => Ok("windows-aarch64-nsis"),
        (MacReadOnly, _) => Err(Fallback::MoveToApplications),
        (LinuxOther, _) => Err(Fallback::Package),
        (WindowsAllUsers, _) => Err(Fallback::AllUsers),
        (WindowsPortable, _) => Err(Fallback::Portable),
        (Unbundled, _) => Err(Fallback::NotInstalled),
    }
}

/// Where releases are read from: GitHub, or in QA builds a local feed
/// (`mod.rs` decides; release builds always use GitHub).
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub api_url: String,
    pub page_prefix: String,
    pub download_prefix: String,
}

impl Source {
    pub fn github() -> Self {
        Self {
            api_url: "https://api.github.com/repos/rawtonic-app/rawtonic/releases?per_page=10"
                .into(),
            page_prefix: "https://github.com/rawtonic-app/rawtonic/releases/tag/".into(),
            download_prefix: DOWNLOAD_PREFIX.into(),
        }
    }

    /// A QA feed at `base` (`http://127.0.0.1:8765`): `releases.json` in the
    /// API's format, pages under `/releases/tag/`, files under
    /// `/releases/download/` (scripts/qa/update_feed.py).
    pub fn qa_feed(base: &str) -> Option<Self> {
        let base = base.trim_end_matches('/');
        let url = reqwest::Url::parse(base).ok()?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return None;
        }
        Some(Self {
            api_url: format!("{base}/releases.json"),
            page_prefix: format!("{base}/releases/tag/"),
            download_prefix: format!("{base}/releases/download/"),
        })
    }

    /// The hosts an in-app update may reach, redirects included.
    pub fn allows_host(&self, host: &str) -> bool {
        let own = reqwest::Url::parse(&self.download_prefix)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));
        own.as_deref() == Some(host)
            || (self.download_prefix == DOWNLOAD_PREFIX && GITHUB_DOWNLOAD_HOSTS.contains(&host))
    }
}

/// GitHub serves release files from github.com through a redirect to its
/// file hosts.
const GITHUB_DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Whether the process runs inside a Flatpak sandbox (`FLATPAK_ID` is set for
/// every Flatpak app; `/.flatpak-info` exists in its sandbox). Flatpak delivers
/// the updates there, so Rawtonic makes no update request of its own.
pub fn in_flatpak(flatpak_id: Option<&str>, info_file_exists: bool) -> bool {
    flatpak_id.is_some_and(|id| !id.is_empty()) || info_file_exists
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
pub fn pick_asset<'a>(assets: &'a [Asset], platform: Platform, prefix: &str) -> Option<&'a Asset> {
    let kinds: &[&str] = match platform.os {
        Os::Mac => &[".dmg"],
        // An all-users copy is updated by the all-users installer; the
        // portable zip by hand from the release page.
        Os::Windows => match platform.install {
            Install::WindowsAllUsers => &[".msi"],
            Install::WindowsPortable => &[],
            _ => &["-setup.exe", ".msi"],
        },
        Os::Linux => match platform.linux {
            // Unknown distributions get no .deb or .rpm they may not install.
            LinuxPackage::AppImage => &[".appimage"],
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
        .filter(|a| a.browser_download_url.starts_with(prefix))
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
    source: &Source,
) -> Result<Option<UpdateInfo>, String> {
    let app_image = platform.install == Install::AppImage;
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
            if !r.html_url.starts_with(&source.page_prefix) {
                return None;
            }
            Some((version, r))
        })
        .max_by(|a, b| a.0.cmp_precedence(&b.0))
        .map(|(version, r)| {
            let asset = pick_asset(&r.assets, platform, &source.download_prefix);
            let offer = in_app_offer(&r, &version, platform, source, asset.is_some());
            UpdateInfo {
                version: version.to_string(),
                download_url: asset.map(|a| a.browser_download_url.clone()),
                asset_name: asset.map(|a| a.name.clone()),
                asset_size: asset.map(|a| a.size).filter(|size| *size > 0),
                release_url: r.html_url,
                app_image,
                notes: cut(r.body.unwrap_or_default()),
                in_app: offer.is_ok(),
                fallback: offer.as_ref().err().copied(),
                offer: offer.ok(),
            }
        }))
}

/// The manifest file every release with in-app updates carries.
pub const MANIFEST_NAME: &str = "latest.json";

/// "Install update" for this release on this copy: its own `latest.json`
/// (never another release's, never a moving `latest` link) and this copy's
/// package key. A release without an installer for this computer (an
/// architecture it does not build) has no update package for it either.
fn in_app_offer(
    r: &Release,
    version: &Version,
    platform: Platform,
    source: &Source,
    has_installer: bool,
) -> Result<Offer, Fallback> {
    let target = updater_target(platform)?;
    if !has_installer {
        return Err(Fallback::NoPackage);
    }
    let package_prefix = format!("{}{}/", source.download_prefix, r.tag_name);
    let manifest_url = format!("{package_prefix}{MANIFEST_NAME}");
    r.assets
        .iter()
        .find(|a| a.name == MANIFEST_NAME && a.browser_download_url == manifest_url)
        .ok_or(Fallback::NoManifest)?;
    Ok(Offer {
        version: version.to_string(),
        manifest_url,
        target,
        package_prefix,
    })
}

/// Whether the package a manifest names belongs to the offered release:
/// fetched from that release's files only, over HTTPS unless it is a QA feed.
pub fn package_url_allowed(url: &str, offer: &Offer, source: &Source) -> bool {
    url.starts_with(&offer.package_prefix)
        && url.len() > offer.package_prefix.len()
        && !url[offer.package_prefix.len()..].contains(['/', '?', '#'])
        && reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| source.allows_host(h)))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn flatpak_is_recognised_by_its_id_or_its_info_file() {
        assert!(in_flatpak(Some("cc.rawtonic.editor"), false));
        assert!(in_flatpak(None, true));
        assert!(!in_flatpak(Some(""), false));
        assert!(!in_flatpak(None, false));
    }

    use super::*;
    const MAC_ARM: Platform = Platform {
        os: Os::Mac,
        arch: Arch::Arm64,
        linux: LinuxPackage::AppImage,
        install: Install::MacApp,
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
        let install = match (os, linux) {
            (Os::Mac, _) => Install::MacApp,
            (Os::Windows, _) => Install::WindowsSetup,
            (Os::Linux, LinuxPackage::AppImage) => Install::AppImage,
            (Os::Linux, _) => Install::LinuxOther,
        };
        Platform {
            os,
            arch,
            linux,
            install,
        }
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
        let packages_only = assets(
            "v1.0.0",
            &["Rawtonic_1.0.0_amd64.deb", "Rawtonic-1.0.0-1.x86_64.rpm"],
        );
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
            (&packages_only, Linux, X64, AppImage, None),
            (
                &packages_only,
                Linux,
                X64,
                Rpm,
                Some("Rawtonic-1.0.0-1.x86_64.rpm"),
            ),
        ] {
            assert_eq!(
                pick_asset(files, platform(os, arch, linux), DOWNLOAD_PREFIX)
                    .map(|a| a.name.as_str()),
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
        assert_eq!(pick_asset(&foreign, MAC_ARM, DOWNLOAD_PREFIX), None);
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
        let update = select_release(json, "0.1.0-alpha.7.1", true, MAC_ARM, &Source::github())
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
        let update = select_release(json, "0.1.0-alpha.7.1", true, intel, &Source::github())
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
        assert!(select_release(br#"[{"tag_name":"v0.1.0+build.2","html_url":"https://github.com/rawtonic-app/rawtonic/releases/tag/v0.1.0","draft":false,"prerelease":false}]"#, "0.1.0+build.1", true, MAC_ARM, &Source::github()).unwrap().is_none());
    }
    #[test]
    fn synthetic_api_fixture_skips_drafts_invalid_tags_and_sorts_by_version() {
        let json = include_bytes!("releases.fixture.json");
        assert_eq!(
            select_release(json, "0.1.0-alpha.3", true, MAC_ARM, &Source::github())
                .unwrap()
                .unwrap()
                .version,
            "0.2.0-alpha.4"
        );
        assert_eq!(
            select_release(json, "0.1.0-alpha.3", false, MAC_ARM, &Source::github())
                .unwrap()
                .unwrap()
                .version,
            "0.1.0"
        );
        assert!(
            select_release(json, "0.2.0", true, MAC_ARM, &Source::github())
                .unwrap()
                .is_none()
        );
        assert!(select_release(b"garbage", "0.1.0", true, MAC_ARM, &Source::github()).is_err());
        assert!(select_release(json, "bad", true, MAC_ARM, &Source::github()).is_err());
    }

    /// A release as the API lists it, with `files` at their GitHub URLs.
    fn release_json(tag: &str, prerelease: bool, files: &[&str]) -> String {
        let assets = files
            .iter()
            .map(|f| {
                format!(
                    r#"{{"name":"{f}","size":10,"browser_download_url":"{DOWNLOAD_PREFIX}{tag}/{f}"}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/rawtonic-app/rawtonic/releases/tag/{tag}","draft":false,"prerelease":{prerelease},"assets":[{assets}]}}"#
        )
    }

    const NEW_FILES: &[&str] = &[
        "Rawtonic_0.2.0_aarch64.dmg",
        "Rawtonic_0.2.0_aarch64.app.tar.gz",
        "Rawtonic_0.2.0_x86_64.dmg",
        "Rawtonic_0.2.0_amd64.AppImage",
        "Rawtonic_0.2.0_x64-setup.exe",
        "Rawtonic_0.2.0_x64.msi",
        "Rawtonic_0.2.0_x64-portable.zip",
        "latest.json",
    ];

    #[test]
    fn the_selected_release_brings_its_own_manifest_and_package_key() {
        // A newer pre-release (skipped by stable users) and an older one.
        let json = format!(
            "[{},{},{}]",
            release_json("v0.3.0-beta.1", true, NEW_FILES),
            release_json("v0.2.0", false, NEW_FILES),
            release_json("v0.1.0", false, NEW_FILES)
        );
        let github = Source::github();
        let stable = select_release(json.as_bytes(), "0.1.0", false, MAC_ARM, &github)
            .unwrap()
            .unwrap();
        assert_eq!(stable.version, "0.2.0");
        assert!(stable.in_app);
        assert_eq!(stable.fallback, None);
        assert_eq!(
            stable.offer,
            Some(Offer {
                version: "0.2.0".into(),
                manifest_url:
                    "https://github.com/rawtonic-app/rawtonic/releases/download/v0.2.0/latest.json"
                        .into(),
                target: "darwin-aarch64-app",
                package_prefix:
                    "https://github.com/rawtonic-app/rawtonic/releases/download/v0.2.0/".into(),
            })
        );
        let pre = select_release(json.as_bytes(), "0.1.0", true, MAC_ARM, &github)
            .unwrap()
            .unwrap();
        assert_eq!(
            pre.offer.unwrap().manifest_url,
            "https://github.com/rawtonic-app/rawtonic/releases/download/v0.3.0-beta.1/latest.json"
        );
        // The offer never reaches the webview.
        let sent = serde_json::to_value(&stable).unwrap();
        assert!(sent.get("offer").is_none());
        assert_eq!(sent["inApp"], true);
    }

    #[test]
    fn releases_without_a_manifest_fall_back_to_the_download_link() {
        let files: Vec<&str> = NEW_FILES
            .iter()
            .copied()
            .filter(|f| *f != "latest.json")
            .collect();
        let json = format!("[{}]", release_json("v0.2.0", false, &files));
        let update = select_release(json.as_bytes(), "0.1.0", false, MAC_ARM, &Source::github())
            .unwrap()
            .unwrap();
        assert!(!update.in_app);
        assert_eq!(update.fallback, Some(Fallback::NoManifest));
        assert_eq!(update.offer, None);
        assert_eq!(
            update.asset_name.as_deref(),
            Some("Rawtonic_0.2.0_aarch64.dmg")
        );
        // A manifest whose URL points at another release is not this release's.
        let json = format!("[{}]", release_json("v0.2.0", false, NEW_FILES))
            .replace("v0.2.0/latest.json", "v0.1.0/latest.json");
        let update = select_release(json.as_bytes(), "0.1.0", false, MAC_ARM, &Source::github())
            .unwrap()
            .unwrap();
        assert_eq!(update.fallback, Some(Fallback::NoManifest));
    }

    #[test]
    fn each_install_kind_updates_in_app_or_says_why_not() {
        use LinuxPackage::{AppImage as Ai, Deb};
        use {Arch::*, Install::*, Os::*};
        let json = format!("[{}]", release_json("v0.2.0", false, NEW_FILES));
        for (os, arch, linux, install, target, fallback, download) in [
            (
                Mac,
                Arm64,
                Ai,
                MacApp,
                Some("darwin-aarch64-app"),
                None,
                Some("Rawtonic_0.2.0_aarch64.dmg"),
            ),
            (
                Mac,
                X64,
                Ai,
                MacApp,
                Some("darwin-x86_64-app"),
                None,
                Some("Rawtonic_0.2.0_x86_64.dmg"),
            ),
            (
                Mac,
                Arm64,
                Ai,
                MacReadOnly,
                None,
                Some(Fallback::MoveToApplications),
                Some("Rawtonic_0.2.0_aarch64.dmg"),
            ),
            (
                Mac,
                Arm64,
                Ai,
                Unbundled,
                None,
                Some(Fallback::NotInstalled),
                Some("Rawtonic_0.2.0_aarch64.dmg"),
            ),
            (
                Linux,
                X64,
                Ai,
                Install::AppImage,
                Some("linux-x86_64-appimage"),
                None,
                Some("Rawtonic_0.2.0_amd64.AppImage"),
            ),
            (
                Linux,
                X64,
                Deb,
                LinuxOther,
                None,
                Some(Fallback::Package),
                Some("Rawtonic_0.2.0_amd64.AppImage"),
            ),
            (
                Windows,
                X64,
                Ai,
                WindowsSetup,
                Some("windows-x86_64-nsis"),
                None,
                Some("Rawtonic_0.2.0_x64-setup.exe"),
            ),
            // The all-users copy gets the all-users installer, never the per-user setup beside it.
            (
                Windows,
                X64,
                Ai,
                WindowsAllUsers,
                None,
                Some(Fallback::AllUsers),
                Some("Rawtonic_0.2.0_x64.msi"),
            ),
            (
                Windows,
                X64,
                Ai,
                WindowsPortable,
                None,
                Some(Fallback::Portable),
                None,
            ),
            // No ARM64 build released: no button that could only fail.
            (
                Linux,
                Arm64,
                Ai,
                Install::AppImage,
                None,
                Some(Fallback::NoPackage),
                None,
            ),
            (
                Windows,
                Arm64,
                Ai,
                WindowsSetup,
                None,
                Some(Fallback::NoPackage),
                None,
            ),
        ] {
            let platform = Platform {
                os,
                arch,
                linux,
                install,
            };
            let update =
                select_release(json.as_bytes(), "0.1.0", false, platform, &Source::github())
                    .unwrap()
                    .unwrap();
            assert_eq!(
                update.offer.as_ref().map(|o| o.target),
                target,
                "{install:?}"
            );
            assert_eq!(update.in_app, target.is_some(), "{install:?}");
            assert_eq!(update.fallback, fallback, "{install:?}");
            assert_eq!(update.asset_name.as_deref(), download, "{install:?}");
            assert_eq!(update.app_image, install == Install::AppImage);
        }
    }

    #[test]
    fn only_the_running_appimage_is_trusted() {
        use std::path::Path;
        let file = Some(Path::new("/home/a/Apps/Rawtonic.AppImage"));
        let mount = Some(Path::new("/tmp/.mount_RawtonXYZ"));
        let inside = Path::new("/tmp/.mount_RawtonXYZ/usr/bin/rawtonic-app");
        assert_eq!(running_appimage(true, file, mount, inside).as_deref(), file);
        // A .deb copy started from another AppImage's terminal.
        assert_eq!(
            running_appimage(false, file, mount, Path::new("/usr/bin/rawtonic-app")),
            None
        );
        assert_eq!(
            running_appimage(true, file, mount, Path::new("/usr/bin/rawtonic-app")),
            None
        );
        assert_eq!(running_appimage(false, file, mount, inside), None);
        assert_eq!(running_appimage(true, None, mount, inside), None);
        assert_eq!(running_appimage(true, file, None, inside), None);
    }

    #[test]
    fn install_kind_detection() {
        use std::path::Path;
        let writable = |_: &Path| false;
        assert_eq!(
            mac_install(
                Path::new("/Applications/Rawtonic.app/Contents/MacOS/rawtonic-app"),
                writable
            ),
            Install::MacApp
        );
        assert_eq!(
            mac_install(
                Path::new("/Users/a/Apps/Rawtonic QA.app/Contents/MacOS/rawtonic-app"),
                writable
            ),
            Install::MacApp
        );
        assert_eq!(
            mac_install(
                Path::new("/Volumes/Rawtonic/Rawtonic.app/Contents/MacOS/rawtonic-app"),
                |_| true
            ),
            Install::MacReadOnly
        );
        assert_eq!(
            mac_install(
                Path::new("/Users/a/rawtonic/target/debug/rawtonic-app"),
                writable
            ),
            Install::Unbundled
        );
        assert_eq!(
            mac_install(Path::new("/x/Contents/MacOS/rawtonic-app"), writable),
            Install::Unbundled
        );
        // The read-only check looks at the bundle, not the program inside.
        let mut asked = None;
        mac_install(
            Path::new("/Applications/Rawtonic.app/Contents/MacOS/rawtonic-app"),
            |p| {
                asked = Some(p.to_path_buf());
                false
            },
        );
        assert_eq!(
            asked.as_deref(),
            Some(Path::new("/Applications/Rawtonic.app"))
        );

        let pf = vec![
            r"C:\Program Files".to_owned(),
            r"C:\Program Files (x86)".to_owned(),
            String::new(),
        ];
        for (dir, uninstaller, expected) in [
            (
                r"C:\Users\ana\AppData\Local\Rawtonic",
                true,
                Install::WindowsSetup,
            ),
            (
                r"c:\program files\Rawtonic",
                false,
                Install::WindowsAllUsers,
            ),
            (
                r"C:\Program Files\Rawtonic\",
                false,
                Install::WindowsAllUsers,
            ),
            // A setup someone pointed into Program Files still needs administrator rights.
            (r"C:\Program Files\Rawtonic", true, Install::WindowsAllUsers),
            (
                "C:/Program Files (x86)/Rawtonic",
                false,
                Install::WindowsAllUsers,
            ),
            (
                r"C:\Program Files Tools\Rawtonic",
                true,
                Install::WindowsSetup,
            ),
            // Tauri hands over the canonicalized, verbatim path.
            (
                r"\\?\C:\Program Files\Rawtonic",
                false,
                Install::WindowsAllUsers,
            ),
            (
                r"\\?\C:\Users\ana\AppData\Local\Rawtonic",
                true,
                Install::WindowsSetup,
            ),
            (r"D:\Tools\Rawtonic", false, Install::WindowsPortable),
            (
                r"C:\src\rawtonic\target\release",
                false,
                Install::WindowsPortable,
            ),
        ] {
            assert_eq!(windows_install(dir, uninstaller, &pf), expected, "{dir}");
        }
    }

    #[test]
    fn package_urls_stay_within_the_offered_release() {
        let github = Source::github();
        let offer = Offer {
            version: "0.2.0".into(),
            manifest_url: format!("{DOWNLOAD_PREFIX}v0.2.0/latest.json"),
            target: "darwin-aarch64-app",
            package_prefix: format!("{DOWNLOAD_PREFIX}v0.2.0/"),
        };
        assert!(package_url_allowed(
            &format!("{DOWNLOAD_PREFIX}v0.2.0/Rawtonic_0.2.0_aarch64.app.tar.gz"),
            &offer,
            &github
        ));
        for bad in [
            format!("{DOWNLOAD_PREFIX}v0.1.0/Rawtonic_0.1.0_aarch64.app.tar.gz"),
            format!("{DOWNLOAD_PREFIX}v0.2.0/"),
            format!("{DOWNLOAD_PREFIX}v0.2.0/../v0.1.0/x.tar.gz"),
            format!("{DOWNLOAD_PREFIX}v0.2.0/x.tar.gz?y=1"),
            "https://example.invalid/rawtonic-app/rawtonic/releases/download/v0.2.0/x.tar.gz"
                .into(),
            "http://github.com/rawtonic-app/rawtonic/releases/download/v0.2.0/x.tar.gz".into(),
        ] {
            assert!(!package_url_allowed(&bad, &offer, &github), "{bad}");
        }
        for host in [
            "github.com",
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com",
        ] {
            assert!(github.allows_host(host));
        }
        assert!(!github.allows_host("example.invalid"));
        assert!(!github.allows_host("api.github.com"));
    }

    #[test]
    fn a_qa_feed_is_its_own_source_and_host() {
        let feed = Source::qa_feed("http://127.0.0.1:8765/").unwrap();
        assert_eq!(feed.api_url, "http://127.0.0.1:8765/releases.json");
        assert!(feed.allows_host("127.0.0.1"));
        assert!(!feed.allows_host("github.com"));
        assert_eq!(Source::qa_feed("file:///tmp/x"), None);
        assert_eq!(Source::qa_feed("not a url"), None);
        let tag = "v0.2.0";
        let json = format!(
            r#"[{{"tag_name":"{tag}","html_url":"http://127.0.0.1:8765/releases/tag/{tag}","draft":false,"prerelease":false,"assets":[{{"name":"Rawtonic_0.2.0_aarch64.dmg","size":1,"browser_download_url":"http://127.0.0.1:8765/releases/download/{tag}/Rawtonic_0.2.0_aarch64.dmg"}},{{"name":"latest.json","size":1,"browser_download_url":"http://127.0.0.1:8765/releases/download/{tag}/latest.json"}}]}}]"#
        );
        let update = select_release(json.as_bytes(), "0.1.0", false, MAC_ARM, &feed)
            .unwrap()
            .unwrap();
        assert_eq!(
            update.offer.unwrap().manifest_url,
            "http://127.0.0.1:8765/releases/download/v0.2.0/latest.json"
        );
        // The release build's source ignores a feed's pages.
        assert!(
            select_release(json.as_bytes(), "0.1.0", false, MAC_ARM, &Source::github())
                .unwrap()
                .is_none()
        );
    }
}
