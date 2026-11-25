use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use crate::asset_folders::check_directory;
use crate::jobs;
use crate::setup::{self, Installed, validate_comfy_url};

const STORAGE: &str = ".render-bridge-node-packs";
const PREFIX: &str = "rb-node-";
const LOCK: &str = ".manage-lock";
const MARKER: &str = ".render-bridge-node-pack.json";
const ARCHIVE_LIMIT: u64 = 64 * 1024 * 1024;
const PACK_LIMIT: usize = 8192;
const SOURCE_ERROR: &str =
    "Use a public GitHub repository URL and the full 40-character commit SHA.";
const FOLDER_ERROR: &str =
    "Node management needs a saved ComfyUI installation with a regular local custom_nodes folder.";
const CONFLICT: &str =
    "A folder with this pack's name already exists. Existing node packs are preserved.";
const ARCHIVE_ERROR: &str = "The selected commit could not be staged as a safe ComfyUI node pack. Check the source and commit.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    owner: String,
    repo: String,
    pub commit: String,
}

impl Source {
    pub fn parse(repository: &str, commit: &str) -> Result<Self, String> {
        let path = repository
            .trim()
            .strip_prefix("https://github.com/")
            .ok_or(SOURCE_ERROR)?
            .trim_end_matches('/');
        let (owner, repo) = path.split_once('/').ok_or(SOURCE_ERROR)?;
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        let valid_name = |value: &str, limit: usize| {
            !value.is_empty() && value.len() <= limit && value.as_bytes()[0].is_ascii_alphanumeric()
        };
        if !valid_name(owner, 39)
            || !owner
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-')
            || owner.ends_with('-')
            || !valid_name(repo, 100)
            || repo.ends_with('.')
            || repo.to_ascii_lowercase().ends_with(".git")
            || !repo
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'_' | b'-'))
        {
            return Err(SOURCE_ERROR.into());
        }
        let commit = commit.trim();
        if commit.len() != 40 || !commit.bytes().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(SOURCE_ERROR.into());
        }
        Ok(Self {
            owner: owner.to_ascii_lowercase(),
            repo: repo.to_ascii_lowercase(),
            commit: commit.to_ascii_lowercase(),
        })
    }

    pub fn repository(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }

    pub fn tree(&self) -> String {
        format!("{}/tree/{}", self.repository(), self.commit)
    }

    pub fn file(&self, name: &str) -> Option<url::Url> {
        let mut url =
            url::Url::parse(&format!("{}/blob/{}", self.repository(), self.commit)).ok()?;
        url.path_segments_mut().ok()?.push(name);
        Some(url)
    }

    fn archive(&self) -> String {
        format!(
            "https://codeload.github.com/{}/{}/zip/{}",
            self.owner, self.repo, self.commit
        )
    }

    fn folder(&self) -> String {
        format!("{PREFIX}{}-{}-{}", self.owner.len(), self.owner, self.repo)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub root: PathBuf,
    pub origin: String,
    python: PathBuf,
}

impl Target {
    pub fn from_installation(backend: &str, installed: &Installed) -> Result<Self, String> {
        let settings = setup::saved_install_settings(installed)?;
        let origin = validate_comfy_url(backend)?;
        if origin != settings.comfy_url {
            return Err(
                "The selected endpoint differs from the saved ComfyUI installation.".into(),
            );
        }
        Self::from_verified_local(
            &origin,
            Path::new(&settings.comfy_root),
            &setup::venv_python(&installed.app_dir),
        )
    }

    pub(crate) fn from_verified_local(
        backend: &str,
        root: &Path,
        python: &Path,
    ) -> Result<Self, String> {
        let origin = validate_comfy_url(backend)?;
        checked_local_directory(&root.join("custom_nodes"))?;
        if !python.is_absolute() || !python.is_file() {
            return Err("Repair Render Bridge before staging a node pack.".into());
        }
        Ok(Self {
            root: root.to_owned(),
            origin,
            python: python.to_owned(),
        })
    }

    pub fn destination(&self, source: &Source) -> PathBuf {
        self.root.join("custom_nodes").join(source.folder())
    }

    pub fn inventory(&self) -> Result<Vec<ManagedPack>, String> {
        let enabled = self.root.join("custom_nodes");
        let disabled = self.root.join(STORAGE);
        checked_local_directory(&enabled)?;
        let mut packs = BTreeMap::new();
        for (parent, enabled) in [(&enabled, true), (&disabled, false)] {
            match fs::symlink_metadata(parent) {
                Err(error) if !enabled && error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(FOLDER_ERROR.into()),
                Ok(_) => checked_local_directory(parent)?,
            }
            let entries = fs::read_dir(parent).map_err(|_| FOLDER_ERROR)?;
            for (index, entry) in entries.enumerate() {
                if index >= PACK_LIMIT {
                    return Err("The managed node pack folder exceeds the inventory limit.".into());
                }
                let path = entry.map_err(|_| FOLDER_ERROR)?.path();
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if !name.starts_with(PREFIX) {
                    continue;
                }
                if let Ok(source) = read_marker(&path) {
                    if packs
                        .insert(name.to_owned(), ManagedPack { source, enabled })
                        .is_some()
                    {
                        return Err("A managed pack exists in both active and disabled folders. Resolve the duplicate before changing packs.".into());
                    }
                }
            }
        }
        Ok(packs.into_values().collect())
    }

    pub fn stage(&self, source: Source) -> Result<StagedPack, String> {
        let enabled = self.root.join("custom_nodes");
        let storage = self.storage()?;
        let _pins = pin_directories(&[&enabled, &storage])?;
        self.check_available(&source)?;
        let stage = tempfile::Builder::new()
            .prefix(".review-")
            .tempdir_in(&storage)
            .map_err(|_| "Could not create a temporary node review folder.")?;
        let archive = stage.path().join("pack.zip");
        download(&source, &archive)?;
        let payload = stage.path().join("payload");
        fs::create_dir(&payload).map_err(|_| ARCHIVE_ERROR)?;
        let inspection = setup::quiet_command(&self.python)
            .current_dir(stage.path())
            .args([
                "-I",
                "-S",
                "-X",
                "utf8",
                "-c",
                include_str!("node_archive.py"),
            ])
            .arg(&archive)
            .arg(&payload)
            .arg(format!("{}-{}", source.repo, source.commit))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| "Could not start the installed Python archive reader.")?;
        if !inspection.status.success() {
            return Err(ARCHIVE_ERROR.into());
        }
        let review: Review =
            serde_json::from_slice(&inspection.stdout).map_err(|_| ARCHIVE_ERROR)?;
        fs::remove_file(archive).map_err(|_| ARCHIVE_ERROR)?;
        Ok(StagedPack {
            stage,
            source,
            target: self.clone(),
            review,
        })
    }

    pub fn install(&self, stage: StagedPack) -> Result<(), String> {
        if self != &stage.target {
            return Err("The selected ComfyUI installation changed. Stage the pack again.".into());
        }
        let enabled = self.root.join("custom_nodes");
        let storage = self.storage()?;
        let _pins = pin_directories(&[&enabled, &storage, stage.stage.path()])?;
        let _lock = management_lock(&storage)?;
        self.check_available(&stage.source)?;
        let payload = stage.stage.path().join("payload");
        checked_local_directory(&payload)?;
        write_marker(&payload, &stage.source)?;
        publish(&payload, &self.destination(&stage.source))
    }

    pub fn toggle(&self, source: &Source, enable: bool) -> Result<(), String> {
        let enabled = self.root.join("custom_nodes");
        let disabled = self.storage()?;
        let _pins = pin_directories(&[&enabled, &disabled])?;
        let _lock = management_lock(&disabled)?;
        let (from, to) = if enable {
            (disabled, enabled)
        } else {
            (enabled, disabled)
        };
        let from = from.join(source.folder());
        let to = to.join(source.folder());
        if read_marker(&from).as_ref() != Ok(source) {
            return Err("Only node packs installed by Render Bridge can be moved.".into());
        }
        if enable {
            self.check_external_pack(source)?;
        } else {
            expect_absent(&to)?;
            let queue = jobs::check(&self.origin)?;
            if !queue.is_idle() {
                return Err(
                    "Wait for running and queued jobs to finish before disabling the pack.".into(),
                );
            }
        }
        publish(&from, &to)
    }

    fn storage(&self) -> Result<PathBuf, String> {
        checked_local_directory(&self.root.join("custom_nodes"))?;
        let storage = self.root.join(STORAGE);
        let _pins = pin_directories(&[&self.root])?;
        match fs::create_dir(&storage) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("Could not create the managed node pack folder.".into()),
        }
        checked_local_directory(&storage)?;
        Ok(storage)
    }

    fn check_available(&self, source: &Source) -> Result<(), String> {
        expect_absent(&self.destination(source))?;
        expect_absent(&self.root.join(STORAGE).join(source.folder()))?;
        self.check_external_pack(source)
    }

    fn check_external_pack(&self, source: &Source) -> Result<(), String> {
        let entries = fs::read_dir(self.root.join("custom_nodes")).map_err(|_| FOLDER_ERROR)?;
        for (index, entry) in entries.enumerate() {
            if index >= PACK_LIMIT {
                return Err("The node pack folder exceeds the inventory limit.".into());
            }
            let entry = entry.map_err(|_| FOLDER_ERROR)?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(&source.repo))
            {
                return Err("A pack with this repository name already exists in custom_nodes. Keep using the existing pack.".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub files: usize,
    pub dependencies: Vec<String>,
    pub startup_hooks: Vec<String>,
}

pub struct StagedPack {
    stage: TempDir,
    pub source: Source,
    pub target: Target,
    pub review: Review,
}

#[derive(Clone)]
pub struct ManagedPack {
    pub source: Source,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    source: Source,
}

fn write_marker(path: &Path, source: &Source) -> Result<(), String> {
    use std::io::Write as _;
    let data = serde_json::to_vec(&Marker {
        version: 1,
        source: source.clone(),
    })
    .map_err(|_| ARCHIVE_ERROR)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.join(MARKER))
        .map_err(|_| "Could not mark the reviewed node pack as managed.")?;
    file.write_all(&data)
        .and_then(|()| file.sync_all())
        .map_err(|_| "Could not save the node pack ownership record.".into())
}

fn read_marker(path: &Path) -> Result<Source, String> {
    checked_local_directory(path)?;
    let path_marker = path.join(MARKER);
    let metadata = fs::symlink_metadata(&path_marker).map_err(|_| FOLDER_ERROR)?;
    if !metadata.is_file() || is_link(&metadata) || metadata.len() > 4096 {
        return Err(FOLDER_ERROR.into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path_marker)
        .and_then(|file| file.take(4097).read_to_end(&mut bytes))
        .map_err(|_| FOLDER_ERROR)?;
    if bytes.len() > 4096 {
        return Err(FOLDER_ERROR.into());
    }
    let marker: Marker = serde_json::from_slice(&bytes).map_err(|_| FOLDER_ERROR)?;
    if marker.version != 1
        || Source::parse(&marker.source.repository(), &marker.source.commit).as_ref()
            != Ok(&marker.source)
        || path.file_name().and_then(|name| name.to_str()) != Some(&marker.source.folder())
    {
        return Err(FOLDER_ERROR.into());
    }
    Ok(marker.source)
}

fn is_link(metadata: &fs::Metadata) -> bool {
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

fn checked_local_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.parent().is_none()
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err(FOLDER_ERROR.into());
    }
    #[cfg(windows)]
    if !matches!(path.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(FOLDER_ERROR.into());
    }
    let ancestors: Vec<_> = path.ancestors().collect();
    for ancestor in ancestors.into_iter().rev() {
        check_directory(ancestor).map_err(|_| FOLDER_ERROR)?;
    }
    Ok(())
}

#[cfg(windows)]
fn pin_directories(folders: &[&Path]) -> Result<Vec<fs::File>, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut handles = Vec::new();
    for path in folders {
        let ancestors: Vec<_> = path.ancestors().collect();
        for ancestor in ancestors.into_iter().rev() {
            let file = OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES.0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
                .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                .open(ancestor)
                .map_err(|_| FOLDER_ERROR)?;
            let metadata = file.metadata().map_err(|_| FOLDER_ERROR)?;
            if !metadata.is_dir() || is_link(&metadata) {
                return Err(FOLDER_ERROR.into());
            }
            handles.push(file);
        }
    }
    Ok(handles)
}

#[cfg(not(windows))]
fn pin_directories(folders: &[&Path]) -> Result<(), String> {
    folders
        .iter()
        .try_for_each(|path| checked_local_directory(path))
}

fn management_lock(storage: &Path) -> Result<fs::File, String> {
    let path = storage.join(LOCK);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        _ => return Err(FOLDER_ERROR.into()),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|_| FOLDER_ERROR)?;
    let metadata = file.metadata().map_err(|_| FOLDER_ERROR)?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(FOLDER_ERROR.into());
    }
    file.try_lock()
        .map_err(|_| "Another Render Bridge window is changing node packs. Try again.")?;
    Ok(file)
}

fn expect_absent(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(CONFLICT.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(FOLDER_ERROR.into()),
    }
}

fn publish(from: &Path, to: &Path) -> Result<(), String> {
    expect_absent(to)?;
    move_new(from, to).map_err(|_| {
        expect_absent(to).err().unwrap_or_else(|| {
            "Could not move the reviewed pack. Check the folder, permissions, and free space."
                .into()
        })
    })
}

#[cfg(windows)]
fn move_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::MoveFileW;
    use windows::core::PCWSTR;

    let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // MoveFileW fails if the destination already exists.
    unsafe { MoveFileW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr())) }.map_err(io::Error::other)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn move_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn move_new(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "exclusive directory moves are unavailable",
    ))
}

fn download(source: &Source, destination: &Path) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .timeout_global(Some(Duration::from_secs(90)))
        .build()
        .into();
    let mut response = agent
        .get(source.archive())
        .call()
        .map_err(|_| "Could not download the public GitHub commit archive.")?;
    if response.status() != 200 {
        return Err(
            "GitHub did not provide this public commit archive. Check the source and commit."
                .into(),
        );
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|_| ARCHIVE_ERROR)?;
    let size = io::copy(
        &mut response.body_mut().as_reader().take(ARCHIVE_LIMIT + 1),
        &mut file,
    )
    .map_err(|_| "Could not save the commit archive in the review folder.")?;
    if size == 0 || size > ARCHIVE_LIMIT {
        return Err("The archive was empty or exceeded 64 MiB.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::time::Instant;
    use tempfile::tempdir;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn accepts_only_a_public_github_repository_and_full_commit() {
        let source = Source::parse("https://github.com/Ow-ner/Pack.Name.git", REVISION).unwrap();
        assert_eq!(source.repository(), "https://github.com/ow-ner/pack.name");
        assert_eq!(
            source.archive(),
            format!("https://codeload.github.com/ow-ner/pack.name/zip/{REVISION}")
        );
        for url in [
            "https://github.com/owner/pack/releases",
            "https://github.com/owner/pack?private=true",
            "https://github.com:8443/owner/pack",
            "https://user@github.com/owner/pack",
            "https://github.com/other/../owner/pack",
            "https://github.com/owner/%70ack",
            "https://github.com.evil.test/owner/pack",
            "http://github.com/owner/pack",
        ] {
            assert!(Source::parse(url, REVISION).is_err(), "{url}");
        }
        assert!(Source::parse("https://github.com/owner/pack", "main").is_err());
    }

    fn with_queue(responses: &[&str], run: impl FnOnce(String)) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        std::thread::scope(|threads| {
            threads.spawn(move || {
                for response in responses {
                    let deadline = Instant::now() + Duration::from_secs(4);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error)
                                if error.kind() == io::ErrorKind::WouldBlock
                                    && Instant::now() < deadline =>
                            {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => panic!("Expected a queue check: {error}"),
                        }
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut reader = BufReader::new(&mut stream);
                    let mut line = String::new();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    assert!(line.starts_with("GET /api/jobs?") || line.starts_with("GET /queue "));
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                            break;
                        }
                    }
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                        response.len(),
                    )
                    .unwrap();
                }
            });
            run(origin);
        });
    }

    #[test]
    fn refuses_to_disable_while_queue_is_busy_or_unknown() {
        let busy = r#"{"jobs":[{"status":"pending"}],"pagination":{"offset":0,"total":1,"has_more":false}}"#;
        for responses in [&[busy][..], &["{}", "{}"][..]] {
            with_queue(responses, |origin| {
                let temp = tempdir().unwrap();
                let root = temp.path().to_owned();
                let enabled = root.join("custom_nodes");
                fs::create_dir(&enabled).unwrap();
                let target = Target {
                    root,
                    origin,
                    python: PathBuf::new(),
                };
                let source = Source::parse("https://github.com/owner/pack", REVISION).unwrap();
                let active = enabled.join(source.folder());
                fs::create_dir(&active).unwrap();
                write_marker(&active, &source).unwrap();
                assert!(target.toggle(&source, false).is_err());
                assert!(active.is_dir());
            });
        }
    }

    #[test]
    fn existing_manual_pack_with_a_different_case_blocks_install() {
        let temp = tempdir().unwrap();
        let root = temp.path().to_owned();
        let enabled = root.join("custom_nodes");
        fs::create_dir(&enabled).unwrap();
        let target = Target {
            root,
            origin: "http://127.0.0.1:8188".into(),
            python: PathBuf::new(),
        };
        let source = Source::parse("https://github.com/owner/pack", REVISION).unwrap();
        let storage = target.storage().unwrap();
        let stage = tempfile::Builder::new().tempdir_in(&storage).unwrap();
        fs::create_dir(stage.path().join("payload")).unwrap();
        assert!(target.check_available(&source).is_ok());
        let manual = enabled.join("PaCk");
        fs::create_dir(&manual).unwrap();
        fs::write(manual.join("keep.py"), b"# other installer").unwrap();
        assert!(target.check_available(&source).is_err());
        let staged = StagedPack {
            stage,
            source: source.clone(),
            target: target.clone(),
            review: Review {
                files: 1,
                dependencies: Vec::new(),
                startup_hooks: Vec::new(),
            },
        };
        assert!(target.install(staged).is_err());
        assert!(!target.destination(&source).exists());
        assert!(manual.join("keep.py").is_file());
    }
    #[test]
    fn toggles_only_owned_directories_and_preserves_external_nodes() {
        let temp = tempdir().unwrap();
        let root = temp.path().to_owned();
        let enabled = root.join("custom_nodes");
        let disabled = root.join(STORAGE);
        fs::create_dir(&enabled).unwrap();
        fs::create_dir(&disabled).unwrap();
        let target = Target {
            root,
            origin: "http://127.0.0.1:8188".into(),
            python: PathBuf::new(),
        };
        let source = Source::parse("https://github.com/owner/pack", REVISION).unwrap();
        let active = enabled.join(source.folder());
        let inactive = disabled.join(source.folder());
        fs::create_dir(&active).unwrap();
        fs::write(active.join("__init__.py"), b"# fixture").unwrap();
        assert!(target.toggle(&source, false).is_err());
        assert!(active.is_dir());
        assert!(!inactive.exists());

        write_marker(&active, &source).unwrap();
        fs::create_dir(&inactive).unwrap();
        assert!(target.toggle(&source, false).is_err());
        assert!(active.is_dir());
        fs::remove_dir(&inactive).unwrap();

        let idle = r#"{"jobs":[],"pagination":{"offset":0,"total":0,"has_more":false}}"#;
        with_queue(&[idle], |origin| {
            let target = Target {
                origin,
                ..target.clone()
            };
            target.toggle(&source, false).unwrap();
        });
        assert!(!active.exists());
        assert!(inactive.join("__init__.py").is_file());
        assert_eq!(target.inventory().unwrap().len(), 1);
        target.toggle(&source, true).unwrap();
        assert!(active.join("__init__.py").is_file());
    }
}
