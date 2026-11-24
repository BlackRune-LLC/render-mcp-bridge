use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

const FILE_NAME: &str = "asset-folders.json";
const MAX_PREFERENCES_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AssetFolders {
    pub library: Option<PathBuf>,
    pub backup: Option<PathBuf>,
}

pub fn load(data_dir: &Path) -> Result<AssetFolders, String> {
    let file = match File::open(data_path(data_dir)?) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AssetFolders::default());
        }
        Err(_) => return Err("Could not read the asset folder preferences.".into()),
    };
    let mut content = Vec::new();
    file.take(MAX_PREFERENCES_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|_| "Could not read the asset folder preferences.")?;
    if content.len() as u64 > MAX_PREFERENCES_BYTES {
        return Err("The asset folder preferences are too large.".into());
    }
    serde_json::from_slice(&content)
        .map_err(|_| "The asset folder preferences could not be read.".into())
}

pub fn save(data_dir: &Path, folders: &AssetFolders) -> Result<(), String> {
    let path = data_path(data_dir)?;
    // Validate paths before committing preferences, even if a caller did not
    // already compare them with the active ComfyUI folders.
    target_paths(folders)?;
    let mut content = serde_json::to_vec_pretty(folders)
        .map_err(|_| "The asset folder preferences could not be encoded.")?;
    content.push(b'\n');
    if content.len() as u64 > MAX_PREFERENCES_BYTES {
        return Err("The asset folder preferences are too large.".into());
    }
    crate::setup::create_private_dir(data_dir)
        .and_then(|()| crate::setup::atomic_write(&path, &content))
        .map_err(|_| "Could not save the asset folder preferences.".into())
}

fn data_path(data_dir: &Path) -> Result<PathBuf, String> {
    if !data_dir.is_absolute() {
        return Err("Use an absolute Render Bridge data folder path.".into());
    }
    Ok(data_dir.join(FILE_NAME))
}

struct Locations {
    output: PathBuf,
    library: Option<PathBuf>,
    backup: Option<PathBuf>,
}

pub fn validate(
    output_dir: &Path,
    input_dir: &Path,
    data_dir: &Path,
    folders: &AssetFolders,
) -> Result<(), String> {
    if folders.library.is_none() && folders.backup.is_none() {
        return Ok(());
    }
    checked_locations(output_dir, input_dir, data_dir, folders).map(|_| ())
}

fn checked_locations(
    output_dir: &Path,
    input_dir: &Path,
    data_dir: &Path,
    folders: &AssetFolders,
) -> Result<Locations, String> {
    let output = directory(output_dir, "ComfyUI output")?;
    let protected = [
        canonical_location(input_dir).map_err(|_| "Could not locate the ComfyUI input folder.")?,
        canonical_location(data_dir)
            .map_err(|_| "Could not locate the Render Bridge data folder.")?,
    ];
    if protected.iter().any(|root| overlaps(&output, root)) {
        return Err(
            "Choose a ComfyUI output folder separate from input and Render Bridge data.".into(),
        );
    }
    let (library, backup) = target_paths(folders)?;
    for target in library.iter().chain(&backup) {
        if overlaps(target, &output) || protected.iter().any(|root| overlaps(target, root)) {
            return Err(
                "Choose asset folders separate from ComfyUI output, input, and Render Bridge data."
                    .into(),
            );
        }
    }
    Ok(Locations {
        output,
        library,
        backup,
    })
}

fn target_paths(folders: &AssetFolders) -> Result<(Option<PathBuf>, Option<PathBuf>), String> {
    let library = folders
        .library
        .as_deref()
        .map(|path| directory(path, "asset library"))
        .transpose()?;
    let backup = folders
        .backup
        .as_deref()
        .map(|path| directory(path, "asset backup"))
        .transpose()?;
    if let (Some(library), Some(backup)) = (&library, &backup)
        && overlaps(library, backup)
    {
        return Err("Choose separate library and backup folders.".into());
    }
    Ok((library, backup))
}

fn directory(path: &Path, name: &str) -> Result<PathBuf, String> {
    let problem = || format!("Choose an existing {name} folder without a link or junction.");
    if !path.is_absolute() {
        return Err(problem());
    }
    check_directory(path).map_err(|_| problem())?;
    fs::canonicalize(path).map_err(|_| problem())
}

fn canonical_location(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an absolute path",
        ));
    }
    let mut ancestor = path;
    let mut suffix = PathBuf::new();
    loop {
        match fs::canonicalize(ancestor) {
            Ok(root) => {
                return Ok(if suffix.as_os_str().is_empty() {
                    root
                } else {
                    root.join(suffix)
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "could not resolve the folder's existing parent",
                    )
                })?;
                suffix = PathBuf::from(name).join(suffix);
                ancestor = ancestor.parent().unwrap_or(ancestor);
            }
            Err(error) => return Err(error),
        }
    }
}

fn overlaps(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        let left = left.as_os_str().to_string_lossy().to_uppercase();
        let right = right.as_os_str().to_string_lossy().to_uppercase();
        let (left, right) = (Path::new(&left), Path::new(&right));
        left.starts_with(right) || right.starts_with(left)
    }
    #[cfg(not(windows))]
    {
        left.starts_with(right) || right.starts_with(left)
    }
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub(crate) fn check_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_link(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular directory",
        ));
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct CopySummary {
    library: Option<usize>,
    backup: Option<usize>,
    existing: usize,
    links: usize,
    problems: usize,
    first_problem: Option<String>,
}

impl CopySummary {
    pub fn message(&self) -> String {
        let mut text = String::new();
        if let Some(library) = self.library {
            text.push_str(&format!("{library} copied to the library."));
        }
        if let Some(backup) = self.backup {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&format!("{backup} copied to the backup."));
        }
        if self.existing > 0 {
            text.push_str(&format!(" Skipped {} existing paths.", self.existing));
        }
        if self.links > 0 {
            text.push_str(&format!(" Skipped {} links or special paths.", self.links));
        }
        if let Some(issue) = &self.first_problem {
            text.push_str(&format!(" {} could not be copied. {issue}", self.problems));
        }
        text
    }

    fn issue(&mut self, path: &Path, error: &io::Error) {
        self.problems += 1;
        if self.first_problem.is_none() {
            let name = if path.as_os_str().is_empty() {
                "output folder".into()
            } else {
                path.to_string_lossy()
            };
            self.first_problem = Some(format!("Could not copy {name} ({error})."));
        }
    }
}

pub fn copy_generated(
    output_dir: &Path,
    input_dir: &Path,
    data_dir: &Path,
    folders: &AssetFolders,
) -> Result<CopySummary, String> {
    if folders.library.is_none() && folders.backup.is_none() {
        return Err(
            "Choose an asset library or backup folder before copying generated assets.".into(),
        );
    }
    let Locations {
        output,
        library,
        backup,
    } = checked_locations(output_dir, input_dir, data_dir, folders)?;
    let mut summary = CopySummary {
        library: library.as_ref().map(|_| 0),
        backup: backup.as_ref().map(|_| 0),
        ..CopySummary::default()
    };
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let dir = output.join(&relative);
        let entries = match check_directory(&dir).and_then(|()| fs::read_dir(&dir)) {
            Ok(entries) => entries,
            Err(error) => {
                summary.issue(&relative, &error);
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    summary.issue(&relative, &error);
                    continue;
                }
            };
            let child = relative.join(entry.file_name());
            match fs::symlink_metadata(entry.path()) {
                Ok(metadata) if is_link(&metadata) => summary.links += 1,
                Ok(metadata) if metadata.is_dir() => pending.push(child),
                Ok(metadata) if metadata.is_file() => {
                    for (target, backup) in [(library.as_ref(), false), (backup.as_ref(), true)] {
                        let Some(target) = target else { continue };
                        match copy_file(&entry.path(), target, &child) {
                            Ok(Copied::New) => {
                                if let Some(count) = if backup {
                                    &mut summary.backup
                                } else {
                                    &mut summary.library
                                } {
                                    *count += 1;
                                }
                            }
                            Ok(Copied::Existing) => summary.existing += 1,
                            Ok(Copied::Link) => summary.links += 1,
                            Err(error) => summary.issue(&child, &error),
                        }
                    }
                }
                Ok(_) => summary.links += 1,
                Err(error) => summary.issue(&child, &error),
            }
        }
    }
    Ok(summary)
}

enum Copied {
    New,
    Existing,
    Link,
}

fn copy_file(source: &Path, target: &Path, relative: &Path) -> io::Result<Copied> {
    let mut source = open_source(source)?;
    let before = source.metadata()?;
    if !before.is_file() || is_link(&before) || multiple_names(&source, &before)? {
        return Ok(Copied::Link);
    }
    let mut parent = target.to_owned();
    check_directory(&parent)?;
    for component in relative
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .components()
    {
        parent.push(component);
        match check_directory(&parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&parent) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                check_directory(&parent)?;
            }
            Err(error) => return Err(error),
        }
    }
    let destination = parent.join(
        relative
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "asset has no filename"))?,
    );
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Ok(Copied::Existing),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    let bytes = io::copy(&mut source, temporary.as_file_mut())?;
    let after = source.metadata()?;
    if bytes != before.len()
        || after.len() != before.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(io::Error::other("the asset changed while copying"));
    }
    temporary.as_file_mut().sync_all()?;
    match temporary.persist_noclobber(destination) {
        Ok(_) => Ok(Copied::New),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(Copied::Existing),
        Err(error) => Err(error.error),
    }
}

fn open_source(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
        options
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

#[cfg(windows)]
fn multiple_names(file: &File, _metadata: &Metadata) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // The File keeps the borrowed handle valid through this query.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }?;
    Ok(info.nNumberOfLinks > 1)
}

#[cfg(unix)]
fn multiple_names(_file: &File, metadata: &Metadata) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(metadata.nlink() > 1)
}

#[cfg(not(any(unix, windows)))]
fn multiple_names(_file: &File, _metadata: &Metadata) -> io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_nested_assets_without_overwriting_and_keeps_the_source() {
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("output");
        let input = fixture.path().join("input");
        let data = fixture.path().join("data");
        let library = fixture.path().join("library");
        let backup = fixture.path().join("backup");
        for path in [
            output.join("nested"),
            input.clone(),
            library.join("nested"),
            backup.clone(),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(output.join("nested/render.png"), b"render").unwrap();
        fs::write(library.join("nested/render.png"), b"existing").unwrap();
        fs::write(input.join("private.png"), b"input").unwrap();
        let folders = AssetFolders {
            library: Some(library.clone()),
            backup: Some(backup.clone()),
        };
        validate(&output, &input, &data, &folders).unwrap();
        assert!(!data.exists());
        let summary = copy_generated(&output, &input, &data, &folders).unwrap();
        assert_eq!(
            (summary.library, summary.backup, summary.existing),
            (Some(0), Some(1), 1)
        );
        assert_eq!(
            fs::read(library.join("nested/render.png")).unwrap(),
            b"existing"
        );
        assert_eq!(
            fs::read(backup.join("nested/render.png")).unwrap(),
            b"render"
        );
        assert_eq!(
            fs::read(output.join("nested/render.png")).unwrap(),
            b"render"
        );
        assert!(!backup.join("private.png").exists());
        save(&data, &folders).unwrap();
        assert_eq!(load(&data).unwrap(), folders);
    }

    #[test]
    fn refuses_overlapping_folders_before_copying() {
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("output");
        let input = fixture.path().join("input");
        let data = fixture.path().join("data");
        let library = fixture.path().join("library");
        for path in [output.join("child"), input.clone(), library.join("child")] {
            fs::create_dir_all(path).unwrap();
        }
        for target in [
            output.join("child"),
            fixture.path().to_owned(),
            input.clone(),
        ] {
            let folders = AssetFolders {
                library: Some(target),
                backup: None,
            };
            assert!(validate(&output, &input, &data, &folders).is_err());
        }
        let folders = AssetFolders {
            library: Some(library.clone()),
            backup: Some(library.join("child")),
        };
        assert!(validate(&output, &input, &data, &folders).is_err());
        assert!(validate(&output, &input, &data, &AssetFolders::default()).is_ok());
        assert!(!data.exists());
    }

    #[cfg(unix)]
    #[test]
    fn skips_a_link_out_of_the_output_tree() {
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("output");
        let input = fixture.path().join("input");
        let library = fixture.path().join("library");
        let data = fixture.path().join("data");
        for path in [&output, &input, &library] {
            fs::create_dir(path).unwrap();
        }
        fs::write(input.join("private.png"), b"input").unwrap();
        std::os::unix::fs::symlink(&input, output.join("input-alias")).unwrap();
        let folders = AssetFolders {
            library: Some(library.clone()),
            backup: None,
        };
        let summary = copy_generated(&output, &input, &data, &folders).unwrap();
        assert_eq!((summary.library, summary.links), (Some(0), 1));
        assert!(!library.join("input-alias").exists());
    }

    #[test]
    fn supports_a_backup_only_and_skips_a_hard_link_to_input() {
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("output");
        let input = fixture.path().join("input");
        let backup = fixture.path().join("backup");
        let data = fixture.path().join("data");
        for path in [&output, &input, &backup] {
            fs::create_dir(path).unwrap();
        }
        fs::write(output.join("render.png"), b"render").unwrap();
        fs::write(input.join("private.png"), b"private").unwrap();
        fs::hard_link(input.join("private.png"), output.join("linked.png")).unwrap();
        let folders = AssetFolders {
            library: None,
            backup: Some(backup.clone()),
        };
        let summary = copy_generated(&output, &input, &data, &folders).unwrap();
        assert_eq!(
            (summary.library, summary.backup, summary.links),
            (None, Some(1), 1)
        );
        assert_eq!(fs::read(backup.join("render.png")).unwrap(), b"render");
        assert!(!backup.join("linked.png").exists());
    }
}
