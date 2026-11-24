use std::fs::{self, Metadata};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ring::digest::{Context, SHA256};
use tempfile::Builder;
use url::Url;

use crate::{asset_folders::check_directory, models::ModelFile};

const HUB: &str = "huggingface.co";
const MAX_REDIRECTS: usize = 6;
const ABORTED: &str = "Model download canceled.";
const BAD_CATALOG: &str = "The model catalog contains an invalid file entry.";
const BAD_FOLDER: &str =
    "The model storage folder could not be verified. Choose a regular local folder.";
const BAD_FILE: &str = "A model destination is inaccessible or is not a regular file.";
const DOWNLOAD_ERROR: &str =
    "The connection to Hugging Face could not complete the model download.";
const VERIFY_ERROR: &str = "The downloaded model does not match the expected size or checksum.";
const STORAGE_ERROR: &str =
    "The model could not be saved. Check free space and folder permissions.";

pub fn install(
    root: &Path,
    files: &[ModelFile],
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<usize, String> {
    check_root(root)?;
    check_cancel(cancelled)?;
    let mut pending = Vec::new();
    let mut total = 0_u64;
    for file in files {
        let path = model_path(root, file)?;
        if existing_model(&path)? || pending.iter().any(|entry: &Pending<'_>| entry.path == path) {
            continue;
        }
        let url = catalog_url(file.download).ok_or(BAD_CATALOG)?;
        let digest = checksum(file.sha256).ok_or(BAD_CATALOG)?;
        if file.size == 0 {
            return Err(BAD_CATALOG.into());
        }
        total = total.checked_add(file.size).ok_or(BAD_CATALOG)?;
        pending.push(Pending {
            path,
            file,
            url,
            digest,
        });
    }
    on_progress(0, total);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(15)))
        .build()
        .into();
    let mut installed = 0;
    let mut finished = 0;
    let mut next_update = Instant::now();
    for entry in pending {
        check_cancel(cancelled)?;
        let category = entry.path.parent().ok_or(BAD_FOLDER)?;
        ensure_directory(category)?;
        #[cfg(windows)]
        let _pins = pin_directories(category)?;
        if !existing_model(&entry.path)? {
            let mut response = request_file(&agent, &entry.url, cancelled)?;
            let reader = response
                .body_mut()
                .with_config()
                .limit(entry.file.size.saturating_add(1))
                .reader();
            if stage(reader, &entry, cancelled, |bytes| {
                let now = Instant::now();
                if now >= next_update {
                    on_progress(finished + bytes, total);
                    next_update = now + Duration::from_millis(100);
                }
            })? {
                installed += 1;
            }
        }
        finished += entry.file.size;
        on_progress(finished, total);
    }
    Ok(installed)
}

struct Pending<'a> {
    path: PathBuf,
    file: &'a ModelFile,
    url: Url,
    digest: [u8; 32],
}

fn model_path(root: &Path, file: &ModelFile) -> Result<PathBuf, &'static str> {
    let category = file
        .folder
        .strip_prefix("models/")
        .and_then(|name| name.strip_suffix('/'))
        .ok_or(BAD_CATALOG)?;
    if !matches!(
        category,
        "checkpoints"
            | "diffusion_models"
            | "text_encoders"
            | "vae"
            | "clip_vision"
            | "background_removal"
            | "geometry_estimation"
            | "loras"
            | "upscale_models"
            | "controlnet"
    ) || !file.filename.ends_with(".safetensors")
        || file.filename.starts_with('.')
        || !file
            .filename
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(BAD_CATALOG);
    }
    Ok(root.join(category).join(file.filename))
}

fn check_root(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path.parent().is_none()
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err(BAD_FOLDER);
    }
    #[cfg(windows)]
    if !matches!(path.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(BAD_FOLDER);
    }
    let ancestors: Vec<_> = path.ancestors().collect();
    for ancestor in ancestors.into_iter().rev() {
        check_or_missing(ancestor)?;
    }
    Ok(())
}

fn check_or_missing(path: &Path) -> Result<(), &'static str> {
    match check_directory(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(BAD_FOLDER),
    }
}

fn ensure_directory(path: &Path) -> Result<(), &'static str> {
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        match check_directory(ancestor) {
            Ok(()) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(_) => return Err(BAD_FOLDER),
        }
        #[cfg(windows)]
        let _pins = ancestor
            .parent()
            .filter(|parent| parent.parent().is_some())
            .map(pin_directories)
            .transpose()?;
        match fs::create_dir(ancestor) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(STORAGE_ERROR),
        }
        check_directory(ancestor).map_err(|_| BAD_FOLDER)?;
    }
    Ok(())
}

fn existing_model(path: &Path) -> Result<bool, &'static str> {
    check_or_missing(path.parent().ok_or(BAD_FOLDER)?)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => Ok(true),
        Ok(_) => Err(BAD_FILE),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(BAD_FILE),
    }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn pin_directories(path: &Path) -> Result<Vec<fs::File>, &'static str> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let ancestors: Vec<_> = path.ancestors().collect();
    ancestors
        .into_iter()
        .rev()
        .map(|ancestor| {
            let file = fs::OpenOptions::new()
                .access_mode(FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
                // Keep the directory at its verified name while permitting publication of a child file.
                .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                .open(ancestor)
                .map_err(|_| BAD_FOLDER)?;
            let metadata = file.metadata().map_err(|_| BAD_FOLDER)?;
            if !metadata.is_dir() || is_link(&metadata) {
                return Err(BAD_FOLDER);
            }
            Ok(file)
        })
        .collect()
}

fn catalog_url(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    if !safe_https(&url) || url.host_str() != Some(HUB) {
        return None;
    }
    let mut segments = url.path_segments()?;
    let owner = segments.next()?;
    let repository = segments.next()?;
    let verb = segments.next()?;
    let revision = segments.next()?;
    let part = segments.next()?;
    let pinned = !owner.is_empty()
        && !repository.is_empty()
        && verb == "resolve"
        && revision.len() == 40
        && revision.bytes().all(|c| c.is_ascii_hexdigit())
        && !part.is_empty();
    pinned.then_some(url)
}

fn safe_https(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
}

fn redirect_url(current: &Url, location: &str) -> Option<Url> {
    let next = current.join(location).ok()?;
    let allowed = matches!(
        next.host_str(),
        Some(
            HUB | "cdn-lfs.huggingface.co"
                | "cdn-lfs.hf.co"
                | "cdn-lfs-us-1.hf.co"
                | "cdn-lfs-eu-1.hf.co"
                | "cas-bridge.xethub.hf.co"
                | "cas-bridge.xethub-eu.hf.co"
                | "cas-server.xethub.hf.co"
                | "cas-server.xethub-eu.hf.co"
                | "transfer.xethub.hf.co"
                | "transfer.xethub-eu.hf.co"
                | "aws.cdn.hf.co"
                | "us.aws.cdn.hf.co"
                | "us-east-1.aws.cdn.hf.co"
                | "us-west-2.aws.cdn.hf.co"
                | "eu-west-3.aws.cdn.hf.co"
                | "ap-southeast-1.aws.cdn.hf.co"
                | "us.gcp.cdn.hf.co"
                | "us-east1.us.gcp.cdn.hf.co"
                | "us-central1.us.gcp.cdn.hf.co"
                | "us-west4.us.gcp.cdn.hf.co"
                | "europe-west4.us.gcp.cdn.hf.co"
                | "asia-southeast1.us.gcp.cdn.hf.co"
        )
    );
    (allowed && safe_https(&next)).then_some(next)
}

fn auth_header() -> Option<String> {
    let token = std::env::var("HF_TOKEN").ok()?;
    let token = token.trim();
    (!token.is_empty()
        && token.len() <= 512
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then(|| format!("Bearer {token}"))
}

fn request_file(
    agent: &ureq::Agent,
    initial: &Url,
    cancelled: &AtomicBool,
) -> Result<ureq::http::Response<ureq::Body>, &'static str> {
    let mut url = initial.clone();
    for hop in 0..=MAX_REDIRECTS {
        check_cancel(cancelled)?;
        let request = || {
            agent
                .get(url.as_str())
                .header("Accept", "application/octet-stream")
                .header("Accept-Encoding", "identity")
                .header("User-Agent", "Render-Bridge")
        };
        let mut response = request().call().map_err(|_| DOWNLOAD_ERROR)?;
        if hop == 0
            && matches!(response.status().as_u16(), 401 | 403)
            && let Some(header) = auth_header()
        {
            drop(response);
            check_cancel(cancelled)?;
            response = request()
                .header("Authorization", header)
                .call()
                .map_err(|_| DOWNLOAD_ERROR)?;
        }
        match response.status().as_u16() {
            200 => return Ok(response),
            301 | 302 | 303 | 307 | 308 if hop < MAX_REDIRECTS => {
                let location = response
                    .headers()
                    .get("Location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or(DOWNLOAD_ERROR)?;
                url = redirect_url(&url, location).ok_or(DOWNLOAD_ERROR)?;
            }
            401 | 403 => {
                return Err(
                    "Hugging Face denied access. Accept the model terms and set an authorized HF_TOKEN if the file is gated.",
                );
            }
            _ => break,
        }
    }
    Err(DOWNLOAD_ERROR)
}

fn checksum(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut decoded = [0; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * index..2 * index + 2], 16).ok()?;
    }
    Some(decoded)
}

fn check_cancel(cancelled: &AtomicBool) -> Result<(), &'static str> {
    if cancelled.load(Ordering::Relaxed) {
        Err(ABORTED)
    } else {
        Ok(())
    }
}

fn stage(
    reader: impl Read,
    entry: &Pending<'_>,
    cancelled: &AtomicBool,
    on_progress: impl FnMut(u64),
) -> Result<bool, &'static str> {
    let category = entry.path.parent().ok_or(BAD_FOLDER)?;
    check_root(category)?;
    let mut temporary = Builder::new()
        .prefix(".render-bridge-model-")
        .suffix(".part")
        .tempfile_in(category)
        .map_err(|_| STORAGE_ERROR)?;
    copy_verified(
        reader,
        temporary.as_file_mut(),
        entry.file.size,
        entry.digest,
        cancelled,
        on_progress,
    )?;
    temporary.as_file().sync_all().map_err(|_| STORAGE_ERROR)?;
    check_cancel(cancelled)?;
    check_root(category)?;
    match temporary.persist_noclobber(&entry.path) {
        Ok(_) => Ok(true),
        Err(_) if existing_model(&entry.path)? => Ok(false),
        Err(_) => Err(STORAGE_ERROR),
    }
}

fn copy_verified(
    mut reader: impl Read,
    mut writer: impl Write,
    size: u64,
    expected: [u8; 32],
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> Result<(), &'static str> {
    let mut hash = Context::new(&SHA256);
    let mut total = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        check_cancel(cancelled)?;
        let count = reader.read(&mut buffer).map_err(|_| DOWNLOAD_ERROR)?;
        if count == 0 {
            break;
        }
        if count as u64 > size - total {
            return Err(VERIFY_ERROR);
        }
        let bytes = &buffer[..count];
        writer.write_all(bytes).map_err(|_| STORAGE_ERROR)?;
        hash.update(bytes);
        total += count as u64;
        on_progress(total);
    }
    if total != size || hash.finish().as_ref() != expected {
        return Err(VERIFY_ERROR);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: ModelFile = ModelFile {
        node: "FixtureLoader",
        field: "name",
        filename: "fixture.safetensors",
        folder: "models/checkpoints/",
        source: "https://huggingface.co/owner/repo",
        download: "https://huggingface.co/owner/repo/resolve/0123456789abcdef0123456789abcdef01234567/fixture.safetensors",
        size: 3,
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    };

    #[test]
    fn permits_pinned_hub_files_and_only_official_https_redirects() {
        let url = catalog_url(FIXTURE.download).unwrap();
        assert!(
            catalog_url(
                &FIXTURE
                    .download
                    .replace("0123456789abcdef0123456789abcdef01234567", "main")
            )
            .is_none()
        );
        for allowed in [
            "/api/resolve-cache/models/owner/repo/file",
            "https://cas-bridge.xethub.hf.co/generated?download=1",
            "https://cdn-lfs-us-1.hf.co/owner/repo/file",
        ] {
            assert!(redirect_url(&url, allowed).is_some());
        }
        for rejected in [
            "http://cdn-lfs-us-1.hf.co/file",
            "https://cdn-lfs-us-1.hf.co.attacker.invalid/file",
            "https://cdn-lfs-us-1.hf.co:444/file",
            "https://name@huggingface.co/file",
            "https://127.0.0.1/file",
        ] {
            assert!(redirect_url(&url, rejected).is_none());
        }
    }

    #[test]
    fn uses_only_catalog_categories_and_plain_model_file_names() {
        let root = Path::new("models");
        assert_eq!(
            model_path(root, &FIXTURE).unwrap(),
            root.join("checkpoints").join(FIXTURE.filename)
        );
        for file in [
            ModelFile {
                filename: "../fixture.safetensors",
                ..FIXTURE
            },
            ModelFile {
                folder: "models/../checkpoints/",
                ..FIXTURE
            },
            ModelFile {
                filename: r"C:\fixture.safetensors",
                ..FIXTURE
            },
            ModelFile {
                filename: "model.safetensors:stream",
                ..FIXTURE
            },
        ] {
            assert!(model_path(root, &file).is_err());
        }
    }

    #[test]
    fn only_publishes_verified_files_and_preserves_existing_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("models");
        let path = model_path(&root, &FIXTURE).unwrap();
        ensure_directory(path.parent().unwrap()).unwrap();
        #[cfg(windows)]
        let _pins = pin_directories(path.parent().unwrap()).unwrap();
        let entry = Pending {
            path: path.clone(),
            file: &FIXTURE,
            url: catalog_url(FIXTURE.download).unwrap(),
            digest: checksum(FIXTURE.sha256).unwrap(),
        };
        let cancelled = AtomicBool::new(false);
        assert!(!existing_model(&path).unwrap());
        for bad in [&b"ab"[..], &b"bad"[..], &b"abcd"[..]] {
            assert_eq!(stage(bad, &entry, &cancelled, |_| {}), Err(VERIFY_ERROR));
        }
        assert_eq!(path.parent().unwrap().read_dir().unwrap().count(), 0);
        assert!(stage(&b"abc"[..], &entry, &cancelled, |_| {}).unwrap());
        assert!(existing_model(&path).unwrap());
        fs::write(&path, b"user file").unwrap();
        assert!(!stage(&b"abc"[..], &entry, &cancelled, |_| {}).unwrap());
        assert_eq!(path.parent().unwrap().read_dir().unwrap().count(), 1);
        assert_eq!(fs::read(path).unwrap(), b"user file");
    }

    #[test]
    fn a_canceled_transfer_removes_its_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let path = model_path(&root, &FIXTURE).unwrap();
        ensure_directory(path.parent().unwrap()).unwrap();
        let entry = Pending {
            path: path.clone(),
            file: &FIXTURE,
            url: catalog_url(FIXTURE.download).unwrap(),
            digest: checksum(FIXTURE.sha256).unwrap(),
        };
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            stage(&b"abc"[..], &entry, &cancelled, |_| cancelled
                .store(true, Ordering::Relaxed)),
            Err(ABORTED)
        );
        assert_eq!(path.parent().unwrap().read_dir().unwrap().count(), 0);
    }
}
