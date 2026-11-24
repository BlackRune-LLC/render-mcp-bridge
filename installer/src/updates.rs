use std::time::Duration;

use serde::Deserialize;

const REPOSITORY: &str = "BlackRune-LLC/render-mcp-bridge";
const USER_AGENT: &str = "RenderBridge";
const INSTALLED_VERSION: &str = env!("CARGO_PKG_VERSION");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const ASSET_NAME: &str = "RenderBridgeSetup.exe";
const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;
const REQUEST_ERROR: &str = "Could not check GitHub for a Render Bridge update. Try again later.";
const RELEASE_ERROR: &str = "GitHub did not return a verified stable Render Bridge release.";
const ASSET_ERROR: &str = "This release does not have verifiable Windows installer metadata.";
#[cfg(windows)]
const DOWNLOAD_ERROR: &str = "Could not download the official Render Bridge installer.";
#[cfg(windows)]
const INTEGRITY_ERROR: &str = "The downloaded installer does not match the GitHub release.";

#[derive(Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
    installer: ReleaseAsset,
}

impl Release {
    pub fn is_installed_version(&self) -> bool {
        self.version == INSTALLED_VERSION
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ReleaseAsset {
    url: String,
    size: u64,
    sha256: [u8; 32],
}

#[derive(Deserialize)]
struct ReleaseMetadata {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<AssetMetadata>,
}

#[derive(Deserialize)]
struct AssetMetadata {
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

pub fn check() -> Result<Option<Release>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(format!(
            "https://api.github.com/repos/{REPOSITORY}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT)
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|_| REQUEST_ERROR.to_owned())?;
    if response.status().as_u16() != 200 {
        return Err(REQUEST_ERROR.into());
    }
    let metadata = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_json()
        .map_err(|_| RELEASE_ERROR.to_owned())?;
    applicable_release(
        metadata,
        INSTALLED_VERSION,
        windows_download::installed_matches,
    )
}

fn applicable_release(
    metadata: ReleaseMetadata,
    installed: &str,
    installed_matches: impl FnOnce(&ReleaseAsset) -> Result<bool, String>,
) -> Result<Option<Release>, String> {
    if metadata.draft
        || metadata.prerelease
        || metadata
            .published_at
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(RELEASE_ERROR.into());
    }
    let version = metadata.tag_name.strip_prefix('v').ok_or(RELEASE_ERROR)?;
    let available = stable_version(version).ok_or(RELEASE_ERROR)?;
    let installed =
        stable_version(installed).ok_or("This Render Bridge version cannot be compared.")?;
    if available < installed {
        return Ok(None);
    }
    let mut installers = metadata
        .assets
        .into_iter()
        .filter(|asset| asset.name == ASSET_NAME);
    let asset = installers.next().ok_or(ASSET_ERROR)?;
    let expected_url = format!(
        "https://github.com/{REPOSITORY}/releases/download/{}/{ASSET_NAME}",
        metadata.tag_name
    );
    if installers.next().is_some()
        || asset.state != "uploaded"
        || !(1..=MAX_ASSET_BYTES).contains(&asset.size)
        || asset.browser_download_url != expected_url
    {
        return Err(ASSET_ERROR.into());
    }
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(parse_sha256)
        .ok_or(ASSET_ERROR)?;
    let installer = ReleaseAsset {
        url: expected_url,
        size: asset.size,
        sha256,
    };
    if available == installed && installed_matches(&installer)? {
        return Ok(None);
    }
    Ok(Some(Release {
        version: version.into(),
        url: format!(
            "https://github.com/{REPOSITORY}/releases/tag/{}",
            metadata.tag_name
        ),
        installer,
    }))
}

fn parse_sha256(digest: &str) -> Option<[u8; 32]> {
    let hex = digest.strip_prefix("sha256:")?.as_bytes();
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(hex.chunks_exact(2)) {
        let high = char::from(pair[0]).to_digit(16)? as u8;
        let low = char::from(pair[1]).to_digit(16)? as u8;
        *byte = high << 4 | low;
    }
    Some(bytes)
}

fn stable_version(version: &str) -> Option<[u64; 3]> {
    fn number(component: &str) -> Option<u64> {
        if (component.len() > 1 && component.starts_with('0'))
            || !component.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        component.parse().ok()
    }

    let mut parts = version.split('.');
    let numbers = [
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    ];
    parts.next().is_none().then_some(numbers)
}

#[cfg(windows)]
mod windows_download {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Seek, Write};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use ring::digest::{Context, SHA256};
    use url::Url;
    use windows::Win32::Foundation::{HANDLE, HWND, INVALID_HANDLE_VALUE};
    use windows::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_DISABLE_MD2_MD4, WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
        WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
        WinVerifyTrust,
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    };
    use windows::core::{HSTRING, PCWSTR};

    const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
    const MAX_REDIRECTS: usize = 4;

    pub(super) fn installed_matches(asset: &ReleaseAsset) -> Result<bool, String> {
        let error = "Could not compare the installed Render Bridge executable.".to_owned();
        let path = std::env::current_exe().map_err(|_| error.clone())?;
        let file = File::open(path).map_err(|_| error)?;
        Ok(copy_verified(file, std::io::sink(), asset).is_ok())
    }

    pub struct StagedInstaller {
        // Keep writes and replacement blocked until the caller starts the
        // installer. Drop the lock before TempDir tries to clean its files.
        _verified: File,
        pub directory: tempfile::TempDir,
        pub path: PathBuf,
    }

    pub fn download(release: &Release) -> Result<StagedInstaller, String> {
        let asset = &release.installer;
        let mut url = Url::parse(&asset.url).map_err(|_| DOWNLOAD_ERROR)?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .build()
            .into();
        let deadline = Instant::now() + DOWNLOAD_TIMEOUT;
        for redirect in 0..=MAX_REDIRECTS {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let mut response = agent
                .get(url.as_str())
                .header("User-Agent", USER_AGENT)
                .header("Accept", "application/octet-stream")
                .config()
                .timeout_global(Some(remaining))
                .build()
                .call()
                .map_err(|_| DOWNLOAD_ERROR)?;
            match response.status().as_u16() {
                200 => {
                    let reader = response
                        .body_mut()
                        .with_config()
                        .limit(asset.size + 1)
                        .reader();
                    return stage(reader, asset);
                }
                301 | 302 | 303 | 307 | 308 if redirect < MAX_REDIRECTS => {
                    let location = response
                        .headers()
                        .get("Location")
                        .and_then(|value| value.to_str().ok())
                        .ok_or(DOWNLOAD_ERROR)?;
                    url = download_redirect(&url, location, &asset.url).ok_or(DOWNLOAD_ERROR)?;
                }
                _ => break,
            }
        }
        Err(DOWNLOAD_ERROR.into())
    }

    fn download_redirect(current: &Url, location: &str, asset_url: &str) -> Option<Url> {
        let next = current.join(location).ok()?;
        let approved = match next.host_str() {
            Some("github.com") => next.as_str() == asset_url,
            Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com") => true,
            _ => false,
        };
        (approved
            && next.scheme() == "https"
            && next.username().is_empty()
            && next.password().is_none()
            && next.port().is_none()
            && next.fragment().is_none())
        .then_some(next)
    }

    fn stage(reader: impl Read, asset: &ReleaseAsset) -> Result<StagedInstaller, String> {
        let staging_error = "Could not stage the Render Bridge installer.";
        let directory = tempfile::Builder::new()
            .prefix("render-bridge-update-")
            .tempdir()
            .map_err(|_| staging_error)?;
        let path = directory.path().join(ASSET_NAME);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|_| staging_error)?;
        copy_verified(reader, &mut file, asset)?;
        file.sync_all().map_err(|_| staging_error)?;
        drop(file);
        let mut verified = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(&path)
            .map_err(|_| staging_error)?;
        let metadata = verified.metadata().map_err(|_| staging_error)?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(staging_error.into());
        }
        copy_verified(&mut verified, std::io::sink(), asset)?;
        verified.rewind().map_err(|_| staging_error)?;
        authenticode(&path, &verified)?;
        Ok(StagedInstaller {
            _verified: verified,
            directory,
            path,
        })
    }

    fn copy_verified(
        mut reader: impl Read,
        mut writer: impl Write,
        asset: &ReleaseAsset,
    ) -> Result<(), String> {
        let mut hash = Context::new(&SHA256);
        let mut written = 0_u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer).map_err(|_| DOWNLOAD_ERROR)?;
            if count == 0 {
                break;
            }
            written += count as u64;
            if written > asset.size {
                return Err(INTEGRITY_ERROR.into());
            }
            let chunk = &buffer[..count];
            hash.update(chunk);
            writer
                .write_all(chunk)
                .map_err(|_| "Could not write the Render Bridge installer.")?;
        }
        if written != asset.size || hash.finish().as_ref() != asset.sha256 {
            return Err(INTEGRITY_ERROR.into());
        }
        Ok(())
    }

    fn authenticode(path: &Path, file: &File) -> Result<(), String> {
        let error = "Windows could not verify the installer's Authenticode signature.";
        let path = HSTRING::from(path.as_os_str());
        let mut info = WINTRUST_FILE_INFO {
            cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: PCWSTR(path.as_ptr()),
            hFile: HANDLE(file.as_raw_handle()),
            ..Default::default()
        };
        let mut trust = WINTRUST_DATA {
            cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
            dwUnionChoice: WTD_CHOICE_FILE,
            Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
            dwStateAction: WTD_STATEACTION_VERIFY,
            dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT | WTD_DISABLE_MD2_MD4,
            ..Default::default()
        };
        let mut policy = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // The handle, path and file info stay live through both calls. The
        // invalid window handle and WTD_UI_NONE keep this check noninteractive.
        let status = unsafe {
            let window = HWND(INVALID_HANDLE_VALUE.0);
            let status = WinVerifyTrust(window, &mut policy, (&raw mut trust).cast());
            trust.dwStateAction = WTD_STATEACTION_CLOSE;
            let _ = WinVerifyTrust(window, &mut policy, (&raw mut trust).cast());
            status
        };
        if status == 0 {
            Ok(())
        } else {
            Err(error.into())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn restricts_download_redirects_to_the_original_asset_and_github_asset_hosts() {
            let original = "https://github.com/BlackRune-LLC/render-mcp-bridge/releases/download/v0.2.0/RenderBridgeSetup.exe";
            let current = Url::parse(original).unwrap();
            for accepted in [
                original,
                "https://release-assets.githubusercontent.com/asset?signature=example",
                "https://objects.githubusercontent.com/asset",
            ] {
                assert!(download_redirect(&current, accepted, original).is_some());
            }
            for rejected in [
                "http://release-assets.githubusercontent.com/asset",
                "https://github.com/other/other/releases/download/v0.2.0/RenderBridgeSetup.exe",
                "https://release-assets.githubusercontent.com.other.test/asset",
                "https://user@release-assets.githubusercontent.com/asset",
                "https://release-assets.githubusercontent.com:8080/asset",
                "https://release-assets.githubusercontent.com/asset#fragment",
                "//example.test/asset",
                "file:///C:/render-bridge.exe",
            ] {
                assert!(
                    download_redirect(&current, rejected, original).is_none(),
                    "{rejected}"
                );
            }
        }

        #[test]
        fn validates_size_and_sha256_before_staging() {
            let expected = b"release asset";
            let sha256 = ring::digest::digest(&SHA256, expected);
            let metadata = ReleaseAsset {
                url: String::new(),
                size: expected.len() as u64,
                sha256: sha256.as_ref().try_into().unwrap(),
            };
            let mut output = Vec::new();
            assert!(copy_verified(&expected[..], &mut output, &metadata).is_ok());
            assert_eq!(output, expected);
            for invalid in [
                b"release".as_slice(),
                b"other payload",
                b"release asset with extra bytes",
            ] {
                assert!(copy_verified(invalid, Vec::new(), &metadata).is_err());
            }
        }

        #[test]
        fn an_unsigned_payload_cannot_be_staged_even_with_the_correct_hash() {
            let payload = b"unsigned test fixture";
            let sha256 = ring::digest::digest(&SHA256, payload);
            let metadata = ReleaseAsset {
                url: String::new(),
                size: payload.len() as u64,
                sha256: sha256.as_ref().try_into().unwrap(),
            };
            assert!(
                stage(&payload[..], &metadata).is_err_and(|error| error.contains("Authenticode"))
            );
        }
    }
}

#[cfg(windows)]
pub use windows_download::{StagedInstaller, download};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn newer_release(
        metadata: ReleaseMetadata,
        installed: &str,
    ) -> Result<Option<Release>, String> {
        applicable_release(metadata, installed, |_| Ok(true))
    }

    #[test]
    fn offers_the_official_asset_for_a_different_build_of_the_same_version() {
        let result = applicable_release(metadata("v0.2.0"), "0.2.0", |asset| {
            assert_eq!(asset.sha256, [1; 32]);
            Ok(false)
        })
        .unwrap()
        .unwrap();
        assert_eq!(result.version, "0.2.0");
    }

    fn metadata(tag: &str) -> ReleaseMetadata {
        serde_json::from_value(json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "published_at": "2026-01-01T00:00:00Z",
            "html_url": "https://example.test/untrusted-release",
            "assets": [{
                "name": ASSET_NAME,
                "state": "uploaded",
                "size": 1024,
                "digest": format!("sha256:{}", "01".repeat(32)),
                "browser_download_url": format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{ASSET_NAME}")
            }]
        }))
        .unwrap()
    }

    #[test]
    fn compares_stable_components_numerically_and_builds_the_owned_link() {
        for tag in ["v0.1.10", "v0.2.0"] {
            assert_eq!(newer_release(metadata(tag), "0.2.0"), Ok(None));
        }
        assert_eq!(
            newer_release(metadata("v0.10.0"), "0.2.0"),
            Ok(Some(Release {
                version: "0.10.0".into(),
                url: "https://github.com/BlackRune-LLC/render-mcp-bridge/releases/tag/v0.10.0"
                    .into(),
                installer: ReleaseAsset {
                    url: "https://github.com/BlackRune-LLC/render-mcp-bridge/releases/download/v0.10.0/RenderBridgeSetup.exe".into(),
                    size: 1024,
                    sha256: [1; 32],
                },
            }))
        );
    }

    #[test]
    fn requires_exactly_one_official_size_and_digest_bound_windows_asset() {
        let mut release = metadata("v1.2.3");
        release.assets[0].browser_download_url =
            "https://example.test/RenderBridgeSetup.exe".into();
        assert!(newer_release(release, "0.2.0").is_err());

        let mut release = metadata("v1.2.3");
        release.assets[0].size = MAX_ASSET_BYTES + 1;
        assert!(newer_release(release, "0.2.0").is_err());

        let mut release = metadata("v1.2.3");
        release.assets[0].digest = Some("sha256:abcd".into());
        assert!(newer_release(release, "0.2.0").is_err());

        let mut release = metadata("v1.2.3");
        release.assets.append(&mut metadata("v1.2.3").assets);
        assert!(newer_release(release, "0.2.0").is_err());
    }

    #[test]
    fn requires_a_published_stable_release() {
        let mut release = metadata("v1.2.3");
        release.draft = true;
        assert!(newer_release(release, "0.2.0").is_err());

        let mut release = metadata("v1.2.3");
        release.prerelease = true;
        assert!(newer_release(release, "0.2.0").is_err());

        let mut release = metadata("v1.2.3");
        release.published_at = None;
        assert!(newer_release(release, "0.2.0").is_err());
    }

    #[test]
    fn rejects_noncanonical_tags_and_untrusted_url_components() {
        for tag in [
            "1.2.3",
            "v1.2",
            "v1..2",
            "v01.2.3",
            "v1.02.3",
            "v1.2.03",
            "v1.2.3-pre",
            "v1.2.3+build",
            "v1.2.3.4",
            "v1.2.3 ",
            "v1.2.3/other",
            "v1.2.3%3F",
            "v1.2.3#other",
            "v1.2.3\n",
            "v+1.2.3",
            "v١.2.3",
            "v18446744073709551616.2.3",
            "https://example.test/v1.2.3",
        ] {
            assert!(newer_release(metadata(tag), "0.2.0").is_err(), "{tag:?}");
        }
    }
}
