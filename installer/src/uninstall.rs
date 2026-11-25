use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{asset_folders, setup, shortcuts};

const INSTALL_OWNER: &str = "render-bridge-install.owner";
const RUNTIME_OWNER: &str = "render-bridge-runtime.owner";
const INSTALL_STAMP: &[u8] = b"Render Bridge installer v1\n";
const RUNTIME_STAMP: &[u8] = b"Render Bridge runtime v1\n";
const MAX_RUNTIME_INVENTORY: u64 = 16 * 1024 * 1024;
const UNOWNED: &str = "This folder has no verified Render Bridge installation. Repair an older installation with this installer first.";
const UNSAFE_FOLDER: &str =
    "Render Bridge could not verify the selected folder. Choose a regular local folder.";
const IN_USE: &str =
    "Exit Render Bridge from the notification area and stop its server, then retry.";
const REMOVE: &str =
    "Could not remove all Render Bridge program files. Close any running copy and retry.";

fn regular_directory(path: &Path) -> Result<(), String> {
    if !local_directory_path(path)
        || path
            .ancestors()
            .any(|path| asset_folders::check_directory(path).is_err())
    {
        return Err(UNSAFE_FOLDER.into());
    }
    Ok(())
}

fn local_directory_path(path: &Path) -> bool {
    use std::path::{Component, Prefix};

    matches!(path.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        && path.is_absolute()
        && path.parent().is_some()
        && !path
            .components()
            .any(|component| component == Component::ParentDir)
}

fn reparse(metadata: &Metadata) -> bool {
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

fn open_regular(path: &Path, directory: bool, delete: bool) -> io::Result<File> {
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
    };

    // Attribute-only opens do not participate in Windows share checking.
    let directory_access = if directory { FILE_LIST_DIRECTORY.0 } else { 0 };
    let access = FILE_READ_ATTRIBUTES.0 | directory_access | if delete { DELETE.0 } else { 0 };
    let flags = FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0;
    // Denying write and delete sharing pins the entry at its verified name and
    // type. The reparse flag finds a junction itself instead of following it.
    let file = fs::OpenOptions::new()
        .access_mode(access)
        .custom_flags(flags)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)?;
    let metadata = file.metadata()?;
    if reparse(&metadata)
        || (if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unexpected entry type",
        ));
    }
    Ok(file)
}

pub(crate) fn pin_local_directories(path: &Path) -> Result<Vec<File>, String> {
    if !local_directory_path(path) {
        return Err(UNSAFE_FOLDER.into());
    }
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    ancestors
        .into_iter()
        .map(|path| open_regular(path, true, false).map_err(|_| UNSAFE_FOLDER.into()))
        .collect()
}

fn remove_owned_entry(root: &Path, path: &Path, directory: bool) -> io::Result<()> {
    use std::path::Component;
    use windows::Win32::Foundation::ERROR_DIR_NOT_EMPTY;
    use windows::core::HRESULT;

    let relative = path
        .strip_prefix(root)
        .ok()
        .filter(|relative| {
            relative.parent().is_some()
                && relative
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unexpected entry location"))?;
    let mut parent = root.to_path_buf();
    let mut pins = Vec::new();
    for component in relative.parent().unwrap_or(Path::new("")).components() {
        parent.push(component);
        pins.push(open_regular(&parent, true, false)?);
    }
    let entry = match open_regular(path, directory, true) {
        Ok(entry) => entry,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match crate::startup::delete_by_handle(&entry) {
        Ok(()) => Ok(()),
        // Only the empty directory itself is owned, and user files inside it
        // may remain after all managed files have been removed.
        Err(error) if directory && error.code() == HRESULT::from_win32(ERROR_DIR_NOT_EMPTY.0) => {
            Ok(())
        }
        Err(error) => Err(io::Error::other(error)),
    }
}

fn owned_file(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .ok()?;
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && !reparse(&metadata) && metadata.len() <= limit)
    {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn marker_matches(path: &Path, stamp: &[u8]) -> bool {
    owned_file(path, stamp.len() as u64).as_deref() == Some(stamp)
}

fn mark(path: &Path, stamp: &[u8]) -> Result<(), String> {
    let error = "Could not record Render Bridge ownership. Existing files were kept.";
    if marker_matches(path, stamp) {
        return Ok(());
    }
    let parent = path.parent().ok_or(error)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(|_| error)?;
    staged.write_all(stamp).map_err(|_| error)?;
    staged.as_file().sync_all().map_err(|_| error)?;
    if staged.persist_noclobber(path).is_err() && !marker_matches(path, stamp) {
        return Err(error.into());
    }
    Ok(())
}

pub(crate) fn is_owned(data: &Path) -> bool {
    regular_directory(data).is_ok() && marker_matches(&data.join(INSTALL_OWNER), INSTALL_STAMP)
}

pub(crate) fn mark_install(data: &Path) -> Result<(), String> {
    regular_directory(data)?;
    mark(&data.join(INSTALL_OWNER), INSTALL_STAMP)
}

pub(crate) fn mark_runtime(runtime: &Path) -> Result<(), String> {
    let error = "Could not record ownership of the staged Render Bridge runtime.";
    let inventory = inventory_environment(runtime)?;
    let mut bytes = RUNTIME_STAMP.to_vec();
    serde_json::to_writer(&mut bytes, &inventory).map_err(|_| error)?;
    if bytes.len() as u64 > MAX_RUNTIME_INVENTORY {
        return Err(error.into());
    }
    mark(&runtime.join(RUNTIME_OWNER), &bytes)
}

struct Runtime {
    folder: PathBuf,
    _pin: File,
    files: Vec<PathBuf>,
    environment: EnvironmentInventory,
}

#[derive(Default, Deserialize, Serialize)]
struct EnvironmentInventory {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

struct Installation {
    data: PathBuf,
    runtimes: Vec<Runtime>,
    monitor: Option<Monitor>,
}

struct Monitor {
    executable: PathBuf,
    _pin: File,
}

fn regular_file(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !reparse(&metadata) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        _ => Err(UNSAFE_FOLDER.into()),
    }
}

fn inventory_environment(runtime: &Path) -> Result<EnvironmentInventory, String> {
    let path = runtime.join(".venv");
    let mut inventory = EnvironmentInventory::default();
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(inventory),
        Ok(metadata) if metadata.is_dir() && !reparse(&metadata) => (),
        _ => return Err(UNSAFE_FOLDER.into()),
    }
    let mut directories = vec![path];
    while let Some(directory) = directories.pop() {
        inventory.directories.push(
            directory
                .strip_prefix(runtime)
                .map_err(|_| UNSAFE_FOLDER)?
                .to_path_buf(),
        );
        for child in fs::read_dir(directory).map_err(|_| UNSAFE_FOLDER)? {
            let child = child.map_err(|_| UNSAFE_FOLDER)?;
            let path = child.path();
            let metadata = fs::symlink_metadata(&path).map_err(|_| UNSAFE_FOLDER)?;
            if reparse(&metadata) {
                return Err(UNSAFE_FOLDER.into());
            }
            if metadata.is_dir() {
                directories.push(path);
            } else if metadata.is_file() {
                inventory.files.push(
                    path.strip_prefix(runtime)
                        .map_err(|_| UNSAFE_FOLDER)?
                        .to_path_buf(),
                );
            } else {
                return Err(UNSAFE_FOLDER.into());
            }
        }
    }
    Ok(inventory)
}

fn installed_environment(folder: &Path, inventory: &[u8]) -> Result<EnvironmentInventory, String> {
    use std::collections::HashSet;
    use std::path::Component;

    let recorded: EnvironmentInventory =
        serde_json::from_slice(inventory).map_err(|_| UNSAFE_FOLDER)?;
    let valid_path = |path: &Path| {
        path.starts_with(".venv")
            && path.components().all(|component| {
                matches!(component, Component::Normal(name)
                    if name.to_str().is_some_and(|name| !name.contains([':', '\0'])))
            })
    };
    if !recorded
        .files
        .iter()
        .chain(&recorded.directories)
        .all(|path| valid_path(path))
    {
        return Err(UNSAFE_FOLDER.into());
    }
    let recorded_files: HashSet<_> = recorded.files.into_iter().collect();
    let recorded_directories: HashSet<_> = recorded.directories.into_iter().collect();
    let mut current = inventory_environment(folder)?;
    current.files.retain(|path| recorded_files.contains(path));
    current
        .directories
        .retain(|path| recorded_directories.contains(path));
    current.directories.reverse();
    Ok(current)
}

fn installation(data: &Path) -> Result<Installation, String> {
    if !is_owned(data) {
        return Err(UNOWNED.into());
    }
    let mut runtimes = Vec::new();
    for entry in fs::read_dir(data).map_err(|_| UNSAFE_FOLDER)? {
        let entry = entry.map_err(|_| UNSAFE_FOLDER)?;
        let folder = entry.path();
        let name = entry.file_name();
        if !name.to_str().is_some_and(setup::is_runtime_name) {
            continue;
        }
        let Ok(pin) = open_regular(&folder, true, false) else {
            continue;
        };
        let Some(inventory) = owned_file(&folder.join(RUNTIME_OWNER), MAX_RUNTIME_INVENTORY) else {
            continue;
        };
        let Some(inventory) = inventory.strip_prefix(RUNTIME_STAMP) else {
            continue;
        };
        let mut files = Vec::new();
        for (name, _) in setup::RUNTIME_FILES {
            let path = folder.join(name);
            if regular_file(&path)? {
                files.push(path);
            }
        }
        let environment = installed_environment(&folder, inventory)?;
        runtimes.push(Runtime {
            folder,
            _pin: pin,
            files,
            environment,
        });
    }
    let monitor = open_regular(&data.join(shortcuts::MONITOR_DIR), true, false)
        .ok()
        .and_then(|pin| {
            shortcuts::existing_monitor_executable(data)
                .ok()
                .map(|executable| Monitor {
                    executable,
                    _pin: pin,
                })
        });
    Ok(Installation {
        data: data.to_path_buf(),
        runtimes,
        monitor,
    })
}

impl Installation {
    fn check_content(&self) -> Result<(), String> {
        if self.runtimes.is_empty() && self.monitor.is_none() {
            return Err("There are no verified Render Bridge program files in this folder.".into());
        }
        Ok(())
    }

    fn remove_files(&self) -> Result<(), String> {
        if let Some(monitor) = &self.monitor {
            remove_owned_entry(&self.data, &monitor.executable, false).map_err(|_| REMOVE)?;
        }
        for runtime in &self.runtimes {
            // Remove the interpreter before any supporting files. Windows
            // rejects deletion of an executable that started after the check.
            let python = setup::venv_python(&runtime.folder);
            let python = python.strip_prefix(&runtime.folder).map_err(|_| REMOVE)?;
            if runtime.environment.files.iter().any(|path| path == python) {
                remove_owned_entry(&runtime.folder, &runtime.folder.join(python), false)
                    .map_err(|_| IN_USE)?;
            }
            for file in runtime
                .environment
                .files
                .iter()
                .filter(|path| *path != python)
            {
                remove_owned_entry(&runtime.folder, &runtime.folder.join(file), false)
                    .map_err(|_| REMOVE)?;
            }
            for folder in &runtime.environment.directories {
                remove_owned_entry(&runtime.folder, &runtime.folder.join(folder), true)
                    .map_err(|_| REMOVE)?;
            }
            for file in &runtime.files {
                remove_owned_entry(&runtime.folder, file, false).map_err(|_| REMOVE)?;
            }
            remove_owned_entry(&runtime.folder, &runtime.folder.join(RUNTIME_OWNER), false)
                .map_err(|_| REMOVE)?;
        }
        Ok(())
    }
}

// Check the explicit selection before enabling removal. No files are changed.
pub(crate) fn inspect(data: &Path) -> Result<(), String> {
    is_owned(data).then_some(()).ok_or_else(|| UNOWNED.into())
}

pub(crate) fn uninstall(data: &Path) -> Result<(), String> {
    let _parents = pin_local_directories(data)?;
    inspect(data)?;
    let _lock = setup::lock_install(data)?;
    let install = installation(data)?;
    install.check_content()?;
    verify_stopped(&install.data)?;
    if let Some(monitor) = &install.monitor {
        shortcuts::remove_monitor_links(&install.data, &monitor.executable)?;
    }
    install.remove_files()
}

fn under_directory(image: &Path, directory: &Path) -> bool {
    // Both paths are case-insensitive Windows paths. Keep the component
    // boundary so another installation sharing a name prefix does not match.
    let image = image.to_string_lossy().to_lowercase();
    let directory = directory.to_string_lossy().to_lowercase();
    image == directory
        || image
            .strip_prefix(&directory)
            .is_some_and(|rest| rest.starts_with('\\'))
}

fn verify_stopped(data: &Path) -> Result<(), String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::{HRESULT, PWSTR};

    let unavailable =
        "Windows could not verify that Render Bridge is stopped. Retry after closing it.";
    let data = fs::canonicalize(data).map_err(|_| unavailable)?;
    let current = fs::canonicalize(std::env::current_exe().map_err(|_| unavailable)?)
        .map_err(|_| unavailable)?;
    if under_directory(&current, &data) {
        return Err(
            "Close this copy and open the installer from outside the selected folder.".into(),
        );
    }
    // Snapshot and image handles are owned until this read-only check ends.
    let snapshot = unsafe {
        let handle = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|_| unavailable)?;
        OwnedHandle::from_raw_handle(handle.0)
    };
    let handle = HANDLE(snapshot.as_raw_handle());
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut next = unsafe { Process32FirstW(handle, &mut entry) };
    let mut buffer = vec![0; 32_768];
    while next.is_ok() {
        // Protected and exited processes cannot belong to this user installation.
        if let Ok(process) = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                entry.th32ProcessID,
            )
        } {
            let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
            let mut length = buffer.len() as u32;
            if unsafe {
                QueryFullProcessImageNameW(
                    HANDLE(process.as_raw_handle()),
                    PROCESS_NAME_WIN32,
                    PWSTR(buffer.as_mut_ptr()),
                    &mut length,
                )
            }
            .is_ok()
            {
                let image = PathBuf::from(OsString::from_wide(&buffer[..length as usize]));
                if fs::canonicalize(image).is_ok_and(|image| under_directory(&image, &data)) {
                    return Err(IN_USE.into());
                }
            }
        }
        next = unsafe { Process32NextW(handle, &mut entry) };
    }
    if next.is_err_and(|error| error.code() != HRESULT::from_win32(ERROR_NO_MORE_FILES.0)) {
        return Err(unavailable.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_current_runtime_and_leaves_unrelated_and_retained_data() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("Render Bridge");
        fs::create_dir(&data).unwrap();
        let runtime = data.join("app-owned");
        fs::create_dir_all(runtime.join(".venv")).unwrap();
        fs::write(runtime.join(".venv/owned-package"), "runtime").unwrap();
        fs::write(runtime.join("comfy_mcp_server.py"), "runtime").unwrap();
        fs::write(runtime.join("settings.json"), "saved settings").unwrap();
        fs::write(runtime.join("keep.txt"), "user file").unwrap();
        fs::write(data.join("token.txt"), "fixture token").unwrap();
        fs::write(data.join("active-runtime"), "app-owned").unwrap();
        fs::create_dir(data.join("app-unknown")).unwrap();
        fs::write(data.join("app-unknown/comfy_mcp_server.py"), "other").unwrap();
        mark_install(&data).unwrap();
        mark_runtime(&runtime).unwrap();
        fs::write(runtime.join(".venv/keep.txt"), "unrelated venv file").unwrap();
        let monitor = shortcuts::install_current_monitor(&data).unwrap();
        let monitor_user_file = monitor.parent().unwrap().join("keep.txt");
        fs::write(&monitor_user_file, "another user file").unwrap();
        let _parents = pin_local_directories(&data).unwrap();
        let plan = installation(&data).unwrap();
        assert_eq!(
            plan.monitor
                .as_ref()
                .map(|monitor| monitor.executable.as_path()),
            Some(monitor.as_path())
        );
        plan.remove_files().unwrap();
        assert!(!monitor.exists());
        assert_eq!(
            fs::read_to_string(monitor_user_file).unwrap(),
            "another user file"
        );
        assert!(!runtime.join(".venv/owned-package").exists());
        assert_eq!(
            fs::read_to_string(runtime.join(".venv/keep.txt")).unwrap(),
            "unrelated venv file"
        );
        assert!(!runtime.join("comfy_mcp_server.py").exists());
        assert!(!runtime.join(RUNTIME_OWNER).exists());
        assert_eq!(
            fs::read_to_string(runtime.join("keep.txt")).unwrap(),
            "user file"
        );
        assert_eq!(
            fs::read_to_string(runtime.join("settings.json")).unwrap(),
            "saved settings"
        );
        assert_eq!(
            fs::read_to_string(data.join("token.txt")).unwrap(),
            "fixture token"
        );
        assert_eq!(
            fs::read_to_string(data.join("active-runtime")).unwrap(),
            "app-owned"
        );
        assert_eq!(
            fs::read_to_string(data.join("app-unknown/comfy_mcp_server.py")).unwrap(),
            "other"
        );
        assert!(is_owned(&data));
        assert!(installation(&data).unwrap().check_content().is_err());
    }

    #[test]
    fn rejects_unowned_or_malformed_installations_before_mutation() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path();
        fs::write(data.join("user.txt"), "untouched").unwrap();
        assert!(inspect(data).is_err());
        fs::write(data.join(INSTALL_OWNER), "someone else").unwrap();
        assert!(mark_install(data).is_err());
        assert_eq!(
            fs::read_to_string(data.join(INSTALL_OWNER)).unwrap(),
            "someone else"
        );
        assert_eq!(
            fs::read_to_string(data.join("user.txt")).unwrap(),
            "untouched"
        );
    }

    #[test]
    fn refuses_to_remove_the_folder_containing_this_executable() {
        let current = std::env::current_exe().unwrap();
        let path = current.parent().unwrap();
        assert!(
            verify_stopped(path)
                .unwrap_err()
                .contains("Close this copy")
        );
    }

    #[test]
    fn rejects_a_runtime_with_an_unverified_environment_without_removing_files() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path();
        let runtime = data.join("app-owned");
        fs::create_dir(&runtime).unwrap();
        fs::write(runtime.join("comfy_mcp_server.py"), "original").unwrap();
        mark_install(data).unwrap();
        mark_runtime(&runtime).unwrap();
        fs::write(runtime.join(".venv"), "unrelated file instead of a venv").unwrap();
        assert!(inspect(data).is_ok());
        assert!(uninstall(data).is_err());
        assert_eq!(
            fs::read_to_string(runtime.join("comfy_mcp_server.py")).unwrap(),
            "original"
        );
        fs::remove_file(runtime.join(".venv")).unwrap();
        assert!(uninstall(data).is_ok());
        assert!(!runtime.join("comfy_mcp_server.py").exists());
    }

    #[test]
    fn validated_directory_chain_cannot_be_renamed_during_removal() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("installation");
        fs::create_dir(&directory).unwrap();
        let replacement = fixture.path().join("different name");
        let pins = pin_local_directories(&directory).unwrap();
        assert!(fs::rename(&directory, &replacement).is_err());
        drop(pins);
        fs::rename(&directory, &replacement).unwrap();
    }

    #[test]
    fn removal_checks_the_leaf_type_on_the_open_handle() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("installation");
        let leaf = root.join("app-owned").join("file");
        fs::create_dir_all(&leaf).unwrap();
        let _parents = pin_local_directories(&root).unwrap();
        let error = remove_owned_entry(&root, &leaf, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(leaf.is_dir());
    }
}
