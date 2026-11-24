use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ring::digest::{Context, SHA256};
use tempfile::{Builder, TempPath};
use url::Url;

use crate::{asset_folders::check_directory, models::ModelFile};

const HUB: &str = "huggingface.co";
const MAX_REDIRECTS: usize = 6;
pub(crate) const ABORTED: &str = "Model operation stopped.";
const BAD_CATALOG: &str = "The model catalog contains an invalid file entry.";
const BAD_FOLDER: &str =
    "The model storage folder could not be verified. Choose a regular local folder.";
const BAD_FILE: &str = "A model destination is inaccessible or is not a regular file.";
const DOWNLOAD_ERROR: &str =
    "The connection to Hugging Face could not complete the model download.";
const VERIFY_ERROR: &str = "The downloaded model does not match the expected size or checksum.";
const STORAGE_ERROR: &str =
    "The model could not be saved. Check free space and folder permissions.";
const INTERRUPTED: &str = "The download stopped early. Select Download missing files to resume.";
const CHANGED: &str = "The original model changed. Verify the file again before repairing it.";

#[derive(Clone)]
pub struct Target {
    file: ModelFile,
    pub path: PathBuf,
    original: Option<[u8; 32]>,
}

impl Target {
    pub fn new(file: ModelFile, path: PathBuf) -> Self {
        Self {
            file,
            path,
            original: None,
        }
    }

    pub fn bytes(&self) -> u64 {
        self.file.size
    }

    pub fn is_repair(&self) -> bool {
        self.original.is_some()
    }

    fn check(&self) -> Result<&Path, &'static str> {
        validate_catalog_file(&self.file)?;
        if self.path.file_name() != Some(self.file.filename.as_ref()) {
            return Err(BAD_CATALOG);
        }
        let directory = self.path.parent().ok_or(BAD_FOLDER)?;
        check_root(directory)?;
        Ok(directory)
    }
}

pub struct Verification {
    pub verified: usize,
    pub repair: Vec<Target>,
}

pub fn verify(
    targets: &[Target],
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<Verification, String> {
    let mut files = Vec::with_capacity(targets.len());
    let mut total = 0_u64;
    for target in targets {
        check_cancel(cancelled)?;
        target.check()?;
        let size = open_model(&target.path, false)?
            .metadata()
            .map_err(|_| BAD_FILE)?
            .len();
        total = total.checked_add(size).ok_or(BAD_FILE)?;
        files.push((target, size));
    }
    let mut report = Verification {
        verified: 0,
        repair: Vec::new(),
    };
    let mut finished = 0;
    let mut next_update = Instant::now();
    on_progress(0, total);
    for (target, size) in files {
        let directory = target.check()?;
        #[cfg(windows)]
        let _pins = pin_directories(directory)?;
        #[cfg(not(windows))]
        let _ = directory;
        let mut file = open_model(&target.path, false)?;
        let state = hash_file(&mut file, cancelled, |count| {
            progress(&mut on_progress, &mut next_update, finished + count, total);
        })?;
        if state.bytes != size {
            return Err(CHANGED.into());
        }
        let digest: [u8; 32] = state
            .hash
            .finish()
            .as_ref()
            .try_into()
            .map_err(|_| BAD_FILE)?;
        if size == target.file.size && Some(digest) == checksum(target.file.sha256) {
            report.verified += 1;
        } else {
            let mut repair = target.clone();
            repair.original = Some(digest);
            report.repair.push(repair);
        }
        finished += size;
        on_progress(finished, total);
    }
    Ok(report)
}

#[derive(Default)]
pub struct InstallOutcome {
    pub files: usize,
    pub backups: Vec<PathBuf>,
}

pub fn install(
    targets: &[Target],
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<InstallOutcome, String> {
    check_cancel(cancelled)?;
    let mut pending = Vec::new();
    let mut total = 0_u64;
    for target in targets {
        target.check()?;
        let exists = existing_model(&target.path)?;
        if target.original.is_some() && !exists {
            return Err(CHANGED.into());
        }
        if (exists && target.original.is_none())
            || pending
                .iter()
                .any(|entry: &Pending<'_>| entry.target.path == target.path)
        {
            continue;
        }
        let entry = Pending::new(target)?;
        if target.is_repair() {
            let mut current = open_model(&target.path, false)?;
            if current.metadata().map_err(|_| BAD_FILE)?.len() == target.file.size
                && hash_file(&mut current, cancelled, |_| {})?
                    .hash
                    .finish()
                    .as_ref()
                    == entry.digest
            {
                // An earlier attempt can finish this file before a subsequent
                // file pauses. Reusing the repair plan leaves this copy intact.
                continue;
            }
        }
        total = total.checked_add(target.file.size).ok_or(BAD_CATALOG)?;
        pending.push(entry);
    }
    preflight(&pending, storage_at)?;
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
    let mut outcome = InstallOutcome::default();
    let mut finished = 0;
    let mut next_update = Instant::now();
    for entry in pending {
        check_cancel(cancelled)?;
        let category = entry.target.check()?;
        ensure_directory(category)?;
        #[cfg(windows)]
        let _pins = pin_directories(category)?;
        if entry.target.original.is_some() || !existing_model(&entry.target.path)? {
            let mut part = open_model(&entry.partial, true)?;
            part.try_lock()
                .map_err(|_| "This model download is open in another Render Bridge window.")?;
            let size = entry.target.file.size;
            let mut state = if part.metadata().map_err(|_| BAD_FILE)?.len() > size {
                reset_partial(&mut part)?
            } else {
                hash_file(&mut part, cancelled, |_| {})?
            };
            // A completed, valid partial can be published after restarting the app.
            let valid = state.bytes == size && state.hash.clone().finish().as_ref() == entry.digest;
            if !valid {
                if state.bytes >= size {
                    state = reset_partial(&mut part)?;
                    preflight(std::slice::from_ref(&entry), storage_at)?;
                }
                let (mut response, offset) =
                    request_file(&agent, &entry.url, cancelled, state.bytes, size)?;
                if offset == 0 && state.bytes != 0 {
                    state = reset_partial(&mut part)?;
                    preflight(std::slice::from_ref(&entry), storage_at)?;
                }
                progress(&mut on_progress, &mut next_update, finished + offset, total);
                let reader = response
                    .body_mut()
                    .with_config()
                    .limit(size - offset + 1)
                    .reader();
                write_partial(reader, &mut part, state, &entry, cancelled, |bytes| {
                    progress(&mut on_progress, &mut next_update, finished + bytes, total);
                })?;
            }
            if let Some(backup) = publish(&entry, &part, cancelled)? {
                outcome.files += 1;
                if let Some(backup) = backup {
                    outcome.backups.push(backup);
                }
            }
        }
        finished += entry.target.file.size;
        on_progress(finished, total);
    }
    Ok(outcome)
}

struct Pending<'a> {
    target: &'a Target,
    partial: PathBuf,
    url: Url,
    digest: [u8; 32],
}

impl<'a> Pending<'a> {
    fn new(target: &'a Target) -> Result<Self, &'static str> {
        let file = &target.file;
        let url = catalog_url(file.download).ok_or(BAD_CATALOG)?;
        let digest = checksum(file.sha256).ok_or(BAD_CATALOG)?;
        if file.size == 0 {
            return Err(BAD_CATALOG);
        }
        let identity = format!(
            "{}\n{}\n{}\n{}",
            file.download, file.sha256, file.size, file.filename
        );
        let key: String = ring::digest::digest(&SHA256, identity.as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let partial = target
            .path
            .with_file_name(format!(".render-bridge-model-{key}.part"));
        Ok(Self {
            target,
            partial,
            url,
            digest,
        })
    }
}

fn validate_catalog_file(file: &ModelFile) -> Result<(), &'static str> {
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
    Ok(())
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

fn open_model(path: &Path, partial: bool) -> Result<File, &'static str> {
    check_root(path.parent().ok_or(BAD_FOLDER)?)?;
    let mut options = OpenOptions::new();
    options.read(true);
    if partial {
        options.write(true).create(true).truncate(false);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
        };
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
        if partial {
            options.share_mode(FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| BAD_FILE)?;
    let metadata = file.metadata().map_err(|_| BAD_FILE)?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(BAD_FILE);
    }
    Ok(file)
}

fn remaining_bytes(entry: &Pending<'_>) -> Result<u64, &'static str> {
    match fs::symlink_metadata(&entry.partial) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => {
            let size = entry.target.file.size;
            Ok(size
                - if metadata.len() <= size {
                    metadata.len()
                } else {
                    0
                })
        }
        Ok(_) => Err(BAD_FILE),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(entry.target.file.size),
        Err(_) => Err(BAD_FILE),
    }
}

fn preflight<V: Ord>(
    pending: &[Pending<'_>],
    mut volume_space: impl FnMut(&Path) -> Result<(V, u64), &'static str>,
) -> Result<(), String> {
    let mut volumes = BTreeMap::new();
    for entry in pending {
        let folder = entry.target.check()?;
        let (volume, available) = volume_space(folder)?;
        let (directory, remaining, free) = volumes
            .entry(volume)
            .or_insert_with(|| (folder, 0_u64, available));
        *remaining = remaining
            .checked_add(remaining_bytes(entry)?)
            .ok_or(BAD_CATALOG)?;
        *free = (*free).min(available);
        if *remaining > *free {
            const GIB: f64 = (1024_u64 * 1024 * 1024) as f64;
            return Err(format!(
                "Not enough free space for the remaining model files at {}. Need {:.2} GiB and have {:.2} GiB.",
                directory.display(),
                *remaining as f64 / GIB,
                *free as f64 / GIB,
            ));
        }
    }
    Ok(())
}

fn storage_directory(path: &Path) -> Result<&Path, &'static str> {
    check_root(path)?;
    path.ancestors()
        .find(|ancestor| check_directory(ancestor).is_ok())
        .ok_or(BAD_FOLDER)
}

#[cfg(windows)]
fn storage_at(path: &Path) -> Result<(Vec<u16>, u64), &'static str> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW};
    use windows::core::PCWSTR;

    let directory: Vec<u16> = storage_directory(path)?
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut volume = vec![0; 32768];
    let mut available = 0_u64;
    // Both buffers are valid for the length of the calls. Free space is the
    // number of bytes available to the current user, including quotas.
    unsafe {
        GetVolumePathNameW(PCWSTR(directory.as_ptr()), &mut volume).map_err(|_| STORAGE_ERROR)?;
        GetDiskFreeSpaceExW(PCWSTR(directory.as_ptr()), Some(&mut available), None, None)
            .map_err(|_| STORAGE_ERROR)?;
    }
    volume.truncate(
        volume
            .iter()
            .position(|value| *value == 0)
            .ok_or(STORAGE_ERROR)?,
    );
    Ok((volume, available))
}

#[cfg(unix)]
fn storage_at(path: &Path) -> Result<(u64, u64), &'static str> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let directory = storage_directory(path)?;
    let device = fs::metadata(directory).map_err(|_| STORAGE_ERROR)?.dev();
    let name = CString::new(directory.as_os_str().as_bytes()).map_err(|_| STORAGE_ERROR)?;
    let mut space = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(name.as_ptr(), space.as_mut_ptr()) } != 0 {
        return Err(STORAGE_ERROR);
    }
    // A successful statvfs fills the structure, and f_bavail excludes reserved blocks.
    let space = unsafe { space.assume_init() };
    Ok((
        device,
        (space.f_bavail as u64).saturating_mul(space.f_frsize as u64),
    ))
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
    offset: u64,
    size: u64,
) -> Result<(ureq::http::Response<ureq::Body>, u64), &'static str> {
    let mut url = initial.clone();
    for hop in 0..=MAX_REDIRECTS {
        check_cancel(cancelled)?;
        let request = || {
            let request = agent
                .get(url.as_str())
                .header("Accept", "application/octet-stream")
                .header("Accept-Encoding", "identity")
                .header("User-Agent", "Render-Bridge");
            if offset != 0 {
                request.header("Range", format!("bytes={offset}-"))
            } else {
                request
            }
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
            status @ (200 | 206) => {
                let start = response_start(status, response.headers(), offset, size)?;
                return Ok((response, start));
            }
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

fn response_start(
    status: u16,
    headers: &ureq::http::HeaderMap,
    offset: u64,
    size: u64,
) -> Result<u64, &'static str> {
    use ureq::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE};

    let value = |name| headers.get(name).and_then(|value| value.to_str().ok());
    if headers.contains_key(CONTENT_ENCODING) && value(CONTENT_ENCODING) != Some("identity") {
        return Err(VERIFY_ERROR);
    }
    let start = match status {
        200 if !headers.contains_key(CONTENT_RANGE) => 0,
        206 if offset > 0
            && offset < size
            && value(CONTENT_RANGE)
                == Some(format!("bytes {offset}-{}/{size}", size - 1).as_str())
            && !value(CONTENT_TYPE)
                .is_some_and(|kind| kind.to_ascii_lowercase().starts_with("multipart/")) =>
        {
            offset
        }
        _ => return Err(VERIFY_ERROR),
    };
    if headers.contains_key(CONTENT_LENGTH)
        && value(CONTENT_LENGTH).and_then(|length| length.parse().ok()) != Some(size - start)
    {
        return Err(VERIFY_ERROR);
    }
    Ok(start)
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

fn progress(callback: &mut impl FnMut(u64, u64), next: &mut Instant, bytes: u64, total: u64) {
    let now = Instant::now();
    if now >= *next {
        callback(bytes, total);
        *next = now + Duration::from_millis(100);
    }
}

struct Prefix {
    bytes: u64,
    hash: Context,
}

fn reset_partial(part: &mut File) -> Result<Prefix, &'static str> {
    part.set_len(0).map_err(|_| STORAGE_ERROR)?;
    part.seek(SeekFrom::Start(0)).map_err(|_| STORAGE_ERROR)?;
    Ok(Prefix {
        bytes: 0,
        hash: Context::new(&SHA256),
    })
}

fn hash_file(
    file: &mut File,
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> Result<Prefix, &'static str> {
    let mut hash = Context::new(&SHA256);
    let size = file.metadata().map_err(|_| BAD_FILE)?.len();
    file.seek(SeekFrom::Start(0)).map_err(|_| BAD_FILE)?;
    let mut total = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        check_cancel(cancelled)?;
        let count = file.read(&mut buffer).map_err(|_| BAD_FILE)?;
        if count == 0 {
            break;
        }
        if count as u64 > size - total {
            return Err(CHANGED);
        }
        hash.update(&buffer[..count]);
        total += count as u64;
        on_progress(total);
    }
    if total != size {
        return Err(CHANGED);
    }
    Ok(Prefix { bytes: total, hash })
}

fn write_partial(
    mut reader: impl Read,
    part: &mut File,
    mut state: Prefix,
    entry: &Pending<'_>,
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> Result<(), &'static str> {
    let size = entry.target.file.size;
    let mut buffer = [0; 64 * 1024];
    loop {
        check_cancel(cancelled)?;
        let count = reader.read(&mut buffer).map_err(|_| INTERRUPTED)?;
        if count == 0 {
            break;
        }
        if count as u64 > size - state.bytes {
            reset_partial(part)?;
            return Err(VERIFY_ERROR);
        }
        let bytes = &buffer[..count];
        part.write_all(bytes).map_err(|_| STORAGE_ERROR)?;
        state.hash.update(bytes);
        state.bytes += count as u64;
        on_progress(state.bytes);
    }
    check_cancel(cancelled)?;
    if state.bytes != size {
        return Err(INTERRUPTED);
    }
    if state.hash.finish().as_ref() != entry.digest {
        reset_partial(part)?;
        return Err(VERIFY_ERROR);
    }
    part.sync_all().map_err(|_| STORAGE_ERROR)
}

fn confirmed_part(entry: &Pending<'_>, part: &File) -> Result<(), &'static str> {
    let reopened = open_model(&entry.partial, false)?;
    if !same_file(part, &reopened)? {
        return Err(BAD_FILE);
    }
    Ok(())
}

#[cfg(windows)]
fn same_file(first: &File, second: &File) -> Result<bool, &'static str> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let identity = |file: &File| -> Result<(u32, u32, u32), &'static str> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // The handles are borrowed from live Rust File objects.
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|_| BAD_FILE)?;
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    };
    Ok(identity(first)? == identity(second)?)
}

#[cfg(unix)]
fn same_file(first: &File, second: &File) -> Result<bool, &'static str> {
    use std::os::unix::fs::MetadataExt;
    let first = first.metadata().map_err(|_| BAD_FILE)?;
    let second = second.metadata().map_err(|_| BAD_FILE)?;
    Ok(first.dev() == second.dev() && first.ino() == second.ino())
}

// Return None when another process already installed the file. The nested
// option holds the path of the retained original after an explicit repair.
fn publish(
    entry: &Pending<'_>,
    part: &File,
    cancelled: &AtomicBool,
) -> Result<Option<Option<PathBuf>>, String> {
    check_cancel(cancelled)?;
    entry.target.check()?;
    confirmed_part(entry, part)?;
    part.sync_all().map_err(|_| STORAGE_ERROR)?;
    let backup = entry
        .target
        .original
        .map(|original| save_backup(entry, original, cancelled))
        .transpose()?;
    let published = (|| {
        check_cancel(cancelled)?;
        confirmed_part(entry, part)?;
        let staged = TempPath::try_from_path(&entry.partial).map_err(|_| STORAGE_ERROR)?;
        match staged.persist_noclobber(&entry.target.path) {
            Ok(()) => Ok(true),
            Err(error) => {
                // Keep the valid partial if publication fails so retry needs
                // no additional network transfer. A competing model wins.
                let _ = error.path.keep();
                if backup.is_none() && existing_model(&entry.target.path)? {
                    Ok(false)
                } else {
                    Err(STORAGE_ERROR)
                }
            }
        }
    })();
    match published {
        Ok(true) => Ok(Some(backup)),
        Ok(false) => Ok(None),
        Err(error) => Err(match backup {
            Some(backup) => restore_backup(&entry.target.path, &backup, error),
            None => error.into(),
        }),
    }
}

fn save_backup(
    entry: &Pending<'_>,
    expected: [u8; 32],
    cancelled: &AtomicBool,
) -> Result<PathBuf, String> {
    let category = entry.target.check()?;
    if !existing_model(&entry.target.path)? {
        return Err(CHANGED.into());
    }
    let directory = Builder::new()
        .prefix(".render-bridge-model-backup-")
        .rand_bytes(16)
        .tempdir_in(category)
        .map_err(|_| STORAGE_ERROR)?;
    let backup = directory
        .path()
        .join(format!("{}.backup", entry.target.file.filename));
    // Stop owning the directory before moving any user file into it. Even
    // if the next step fails, a scope guard must not delete the original.
    let directory = directory.keep();
    if fs::rename(&entry.target.path, &backup).is_err() {
        let _ = fs::remove_dir(&directory);
        return Err(
            "Could not move the original to a backup. Close apps using the model and try again."
                .into(),
        );
    }
    let result = (|| {
        let mut original = open_model(&backup, false)?;
        if hash_file(&mut original, cancelled, |_| {})?
            .hash
            .finish()
            .as_ref()
            != expected
        {
            return Err(CHANGED);
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(backup),
        Err(error) => Err(restore_backup(&entry.target.path, &backup, error)),
    }
}

fn restore_backup(target: &Path, backup: &Path, error: &str) -> String {
    let restored = existing_model(backup).unwrap_or(false) && fs::hard_link(backup, target).is_ok();
    let action = if restored {
        "The original was restored."
    } else {
        "The original was kept separately."
    };
    format!("{} {} Backup at {}", error, action, backup.display())
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
        assert!(validate_catalog_file(&FIXTURE).is_ok());
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
            assert!(validate_catalog_file(&file).is_err());
        }
    }

    fn target(temp: &tempfile::TempDir) -> Target {
        let root = temp.path().canonicalize().unwrap().join("models");
        let path = root.join("fixture-category").join(FIXTURE.filename);
        ensure_directory(path.parent().unwrap()).unwrap();
        Target::new(FIXTURE, path)
    }

    #[test]
    fn an_interrupted_file_is_rehashed_and_resumed_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let target = target(&temp);
        let entry = Pending::new(&target).unwrap();
        let cancelled = AtomicBool::new(false);
        {
            let mut file = open_model(&entry.partial, true).unwrap();
            let state = hash_file(&mut file, &cancelled, |_| {}).unwrap();
            assert_eq!(
                write_partial(&b"a"[..], &mut file, state, &entry, &cancelled, |_| {}),
                Err(INTERRUPTED)
            );
        }
        assert!(!target.path.exists());
        assert_eq!(remaining_bytes(&entry).unwrap(), 2);
        {
            let mut file = open_model(&entry.partial, true).unwrap();
            let state = hash_file(&mut file, &cancelled, |_| {}).unwrap();
            assert_eq!(state.bytes, 1);
            write_partial(&b"bc"[..], &mut file, state, &entry, &cancelled, |_| {}).unwrap();
        }
        assert_eq!(
            install(std::slice::from_ref(&target), &cancelled, |_, _| {})
                .unwrap()
                .files,
            1
        );
        assert_eq!(fs::read(&target.path).unwrap(), b"abc");
        assert!(!entry.partial.exists());
    }

    #[test]
    fn a_canceled_transfer_keeps_its_partial_file_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let target = target(&temp);
        let entry = Pending::new(&target).unwrap();
        let cancelled = AtomicBool::new(false);
        let mut file = open_model(&entry.partial, true).unwrap();
        let state = hash_file(&mut file, &cancelled, |_| {}).unwrap();
        assert_eq!(
            write_partial(&b"abc"[..], &mut file, state, &entry, &cancelled, |_| {
                cancelled.store(true, Ordering::Relaxed);
            }),
            Err(ABORTED)
        );
        assert!(!target.path.exists());
        assert_eq!(fs::read(&entry.partial).unwrap(), b"abc");
    }

    #[test]
    fn checks_the_exact_remaining_range_and_restarts_when_ignored() {
        use ureq::http::{HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert("content-length", HeaderValue::from_static("2"));
        headers.insert("content-range", HeaderValue::from_static("bytes 1-2/3"));
        assert_eq!(response_start(206, &headers, 1, 3), Ok(1));
        assert_eq!(response_start(206, &headers, 0, 3), Err(VERIFY_ERROR));
        assert_eq!(response_start(206, &headers, 1, 4), Err(VERIFY_ERROR));
        headers.insert(
            "content-type",
            HeaderValue::from_static("multipart/byteranges"),
        );
        assert_eq!(response_start(206, &headers, 1, 3), Err(VERIFY_ERROR));
        headers.clear();
        headers.insert("content-length", HeaderValue::from_static("3"));
        assert_eq!(response_start(200, &headers, 1, 3), Ok(0));
    }

    #[test]
    fn a_bad_final_checksum_discards_partial_bytes_and_preserves_other_files() {
        let temp = tempfile::tempdir().unwrap();
        let target = target(&temp);
        let entry = Pending::new(&target).unwrap();
        let cancelled = AtomicBool::new(false);
        let mut part = open_model(&entry.partial, true).unwrap();
        let state = hash_file(&mut part, &cancelled, |_| {}).unwrap();
        fs::write(&target.path, b"local user model").unwrap();
        assert_eq!(
            write_partial(&b"bad"[..], &mut part, state, &entry, &cancelled, |_| {}),
            Err(VERIFY_ERROR)
        );
        assert_eq!(part.metadata().unwrap().len(), 0);
        assert_eq!(
            install(std::slice::from_ref(&target), &cancelled, |_, _| {})
                .unwrap()
                .files,
            0
        );
        assert_eq!(fs::read(&target.path).unwrap(), b"local user model");
    }

    #[test]
    fn repair_verifies_the_expected_original_and_preserves_a_backup() {
        let temp = tempfile::tempdir().unwrap();
        let target = target(&temp);
        let entry = Pending::new(&target).unwrap();
        let cancelled = AtomicBool::new(false);
        fs::write(&target.path, b"old model").unwrap();
        fs::write(&entry.partial, b"abc").unwrap();
        let report = verify(std::slice::from_ref(&target), &cancelled, |_, _| {}).unwrap();
        assert_eq!(report.verified, 0);
        assert_eq!(report.repair.len(), 1);

        let installed = install(&report.repair, &cancelled, |_, _| {}).unwrap();
        assert_eq!(installed.files, 1);
        assert_eq!(installed.backups.len(), 1);
        assert_eq!(fs::read(&installed.backups[0]).unwrap(), b"old model");
        assert_eq!(fs::read(&target.path).unwrap(), b"abc");
        let retried = install(&report.repair, &cancelled, |_, _| {}).unwrap();
        assert_eq!(retried.files, 0);
        assert!(retried.backups.is_empty());
    }

    #[test]
    fn repair_refuses_to_replace_a_changed_model() {
        let temp = tempfile::tempdir().unwrap();
        let target = target(&temp);
        let entry = Pending::new(&target).unwrap();
        let cancelled = AtomicBool::new(false);
        fs::write(&target.path, b"old model").unwrap();
        fs::write(&entry.partial, b"abc").unwrap();
        let report = verify(std::slice::from_ref(&target), &cancelled, |_, _| {}).unwrap();
        fs::write(&target.path, b"a replacement from the user").unwrap();

        let evidence = install(&report.repair, &cancelled, |_, _| {})
            .err()
            .unwrap();
        assert!(evidence.starts_with(CHANGED));
        assert_eq!(
            fs::read(&target.path).unwrap(),
            b"a replacement from the user"
        );
        assert_eq!(fs::read(&entry.partial).unwrap(), b"abc");
    }

    #[test]
    fn checks_remaining_space_for_the_whole_drive_before_transfer() {
        let temp = tempfile::tempdir().unwrap();
        let first = target(&temp);
        let mut second = first.clone();
        second.file.filename = "other.safetensors";
        second.path.set_file_name(second.file.filename);
        let first = Pending::new(&first).unwrap();
        let second = Pending::new(&second).unwrap();
        let error = preflight(&[first, second], |_| Ok((1, 5))).unwrap_err();
        assert!(error.starts_with("Not enough free space"));
    }
}
