use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_DIR_NOT_EMPTY, ERROR_INVALID_PARAMETER, GENERIC_READ, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo,
    SetFileInformationByHandle,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows::core::PWSTR;

use crate::setup::{Installed, quiet_command};
use crate::shortcuts::{existing_monitor_executable, install_current_monitor};

const STAGE_PREFIX: &str = "render-bridge-update-";
const INSTALLER_NAME: &str = "RenderBridgeSetup.exe";
const HANDOFF_MARKER: &str = "render-bridge-update.handoff";
const PROCESS_WAIT_MS: u32 = 12_000;
const MARKER_LIMIT: u64 = 128 * 1024;
const INVALID: &str = "This Render Bridge update handoff could not be verified.";
const CLEANUP_ERROR: &str = "Could not remove the verified temporary Render Bridge installer.";

pub struct UpdateRequest {
    pub data_dir: PathBuf,
    pub cleanup_stage: PathBuf,
    previous_pid: u32,
}

impl UpdateRequest {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let current = std::env::current_exe().map_err(|_| INVALID)?;
        Self::parse_at(
            args,
            &current,
            &std::env::temp_dir(),
            env!("CARGO_PKG_VERSION"),
        )
    }

    fn parse_at(
        args: impl IntoIterator<Item = OsString>,
        current: &Path,
        temp_root: &Path,
        version: &str,
    ) -> Result<Self, String> {
        let [data, pid, advertised] = three_args(args)?;
        if advertised != version {
            return Err("The downloaded installer did not match the advertised version.".into());
        }
        let data_dir = existing_directory(&data)?;
        let previous_pid = other_pid(&pid)?;
        let cleanup_stage = stage_for_installer(current, temp_root)?;
        Ok(Self {
            data_dir,
            cleanup_stage,
            previous_pid,
        })
    }

    pub fn wait_for_previous(&self) -> Result<(), String> {
        let previous = existing_monitor_executable(&self.data_dir)?;
        wait_for_image(self.previous_pid, &previous, PROCESS_WAIT_MS)
    }
}

pub struct FinishRequest {
    pub data_dir: PathBuf,
    cleanup_stage: PathBuf,
    helper_pid: u32,
}

impl FinishRequest {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let [data, pid, stage] = three_args(args)?;
        let data_dir = existing_directory(&data)?;
        if !is_installed_monitor(&data_dir) {
            return Err(INVALID.into());
        }
        let helper_pid = other_pid(&pid)?;
        let cleanup_stage = checked_stage(Path::new(&stage), &std::env::temp_dir())?;
        let marker = fs::File::open(cleanup_stage.join(HANDOFF_MARKER)).map_err(|_| INVALID)?;
        if !owned_marker(&marker, &data_dir, helper_pid) {
            return Err(INVALID.into());
        }
        Ok(Self {
            data_dir,
            cleanup_stage,
            helper_pid,
        })
    }

    pub fn cleanup_after_helper(&self) -> Result<(), String> {
        let executable = self.cleanup_stage.join(INSTALLER_NAME);
        wait_for_image(self.helper_pid, &executable, PROCESS_WAIT_MS)?;
        clean_stage(
            &self.cleanup_stage,
            &std::env::temp_dir(),
            &self.data_dir,
            self.helper_pid,
        )
    }
}

pub fn is_installed_monitor(data: &Path) -> bool {
    std::env::current_exe().is_ok_and(|current| {
        existing_monitor_executable(data).is_ok_and(|expected| same_image(&current, &expected))
    })
}

pub fn replace_monitor(installed: &Installed) -> Result<(), String> {
    install_current_monitor(&installed.data_dir).map(drop)
}

pub fn launch_installed_monitor(data: &Path, cleanup_stage: &Path) -> Result<(), String> {
    let data = existing_directory(data.as_os_str())?;
    let executable = existing_monitor_executable(&data)?;
    let current = std::env::current_exe().map_err(|_| INVALID)?;
    let stage = stage_for_installer(&current, &std::env::temp_dir())?;
    if checked_stage(cleanup_stage, &std::env::temp_dir())? != stage {
        return Err(INVALID.into());
    }
    let pid = std::process::id();
    let marker = stage.join(HANDOFF_MARKER);
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&marker)
    {
        Ok(mut file) => file
            .write_all(&marker_data(&data, pid)?)
            .map_err(|_| INVALID)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !File::open(marker).is_ok_and(|file| owned_marker(&file, &data, pid)) {
                return Err(INVALID.into());
            }
        }
        Err(_) => return Err(INVALID.into()),
    }
    quiet_command(&executable)
        .arg("--finish-update")
        .arg(&data)
        .arg(pid.to_string())
        .arg(stage)
        .current_dir(&data)
        .spawn()
        .map(drop)
        .map_err(|_| "Could not reopen Render Bridge. Open it from Windows Start.".into())
}

fn three_args(args: impl IntoIterator<Item = OsString>) -> Result<[OsString; 3], String> {
    let mut args = args.into_iter();
    let values = [
        args.next().ok_or(INVALID)?,
        args.next().ok_or(INVALID)?,
        args.next().ok_or(INVALID)?,
    ];
    if args.next().is_some() {
        return Err(INVALID.into());
    }
    Ok(values)
}

fn existing_directory(path: &OsStr) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if !path.is_absolute() || !path.is_dir() {
        return Err(INVALID.into());
    }
    // Preserve the spelling in the existing Start and Startup arguments.
    Ok(path.to_owned())
}

fn other_pid(pid: &OsStr) -> Result<u32, String> {
    pid.to_str()
        .and_then(|pid| pid.parse().ok())
        .filter(|pid| *pid != 0 && *pid != std::process::id())
        .ok_or_else(|| INVALID.into())
}

fn plain_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
}

fn checked_stage(stage: &Path, temp_root: &Path) -> Result<PathBuf, String> {
    let valid_name = stage
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            name.strip_prefix(STAGE_PREFIX).is_some_and(|suffix| {
                (1..=32).contains(&suffix.len())
                    && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        });
    if !stage.is_absolute() || !valid_name {
        return Err(INVALID.into());
    }
    let metadata = fs::symlink_metadata(stage).map_err(|_| INVALID)?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(INVALID.into());
    }
    let stage = fs::canonicalize(stage).map_err(|_| INVALID)?;
    let root = fs::canonicalize(temp_root).map_err(|_| INVALID)?;
    if stage.parent() != Some(root.as_path()) {
        return Err(INVALID.into());
    }
    Ok(stage)
}

fn stage_for_installer(executable: &Path, temp_root: &Path) -> Result<PathBuf, String> {
    if executable.file_name() != Some(OsStr::new(INSTALLER_NAME))
        || !fs::symlink_metadata(executable).is_ok_and(|entry| plain_file(&entry))
    {
        return Err(INVALID.into());
    }
    checked_stage(executable.parent().ok_or(INVALID)?, temp_root)
}

fn same_image(actual: &Path, expected: &Path) -> bool {
    match (fs::canonicalize(actual), fs::canonicalize(expected)) {
        (Ok(actual), Ok(expected)) => actual == expected,
        _ => false,
    }
}

struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        // The handle is owned here, regardless of whether the process exited.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn wait_for_image(pid: u32, expected: &Path, wait_ms: u32) -> Result<(), String> {
    const UNVERIFIED: &str =
        "The Render Bridge process could not be verified. Close it and try again.";
    // This opens only a read/wait handle and never signals or terminates a process.
    let process = match unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    } {
        Ok(handle) => Process(handle),
        Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Ok(()),
        Err(_) => return Err(UNVERIFIED.into()),
    };
    let exited = || unsafe { WaitForSingleObject(process.0, 0) } == WAIT_OBJECT_0;
    if exited() {
        return Ok(());
    }
    let mut path = vec![0; 32_768];
    let mut len = path.len() as u32;
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut len,
        )
    };
    if exited() {
        return Ok(());
    }
    if queried.is_err()
        || !same_image(
            &PathBuf::from(OsString::from_wide(&path[..len as usize])),
            expected,
        )
    {
        return Err(UNVERIFIED.into());
    }
    if unsafe { WaitForSingleObject(process.0, wait_ms) } != WAIT_OBJECT_0 {
        return Err(
            "Render Bridge is still closing. Exit it from the notification area, then try again."
                .into(),
        );
    }
    Ok(())
}

fn marker_data(data: &Path, pid: u32) -> Result<Vec<u8>, String> {
    let canonical = fs::canonicalize(data).map_err(|_| INVALID)?;
    let data = canonical.to_str().ok_or(INVALID)?;
    Ok(format!("Render Bridge update handoff v1\n{pid}\n{data}").into_bytes())
}

fn owned_marker(file: &File, data: &Path, pid: u32) -> bool {
    let Ok(expected) = marker_data(data, pid) else {
        return false;
    };
    if !file
        .metadata()
        .is_ok_and(|metadata| plain_file(&metadata) && metadata.len() == expected.len() as u64)
    {
        return false;
    }
    let mut actual = Vec::new();
    file.take(MARKER_LIMIT).read_to_end(&mut actual).is_ok() && actual == expected
}

fn clean_stage(stage: &Path, root: &Path, data: &Path, pid: u32) -> Result<(), String> {
    let stage = checked_stage(stage, root)?;
    let open = |path: &Path, flags: u32| {
        OpenOptions::new()
            .access_mode(GENERIC_READ.0 | DELETE.0)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | flags)
            .open(path)
            .map_err(|_| CLEANUP_ERROR)
    };
    // Holding the directory without delete sharing prevents it being renamed
    // while we open the exact two files. No recursive cleanup is performed.
    let directory = open(&stage, FILE_FLAG_BACKUP_SEMANTICS.0)?;
    let metadata = directory.metadata().map_err(|_| CLEANUP_ERROR)?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(INVALID.into());
    }
    let marker = open(&stage.join(HANDOFF_MARKER), 0)?;
    let executable = open(&stage.join(INSTALLER_NAME), 0)?;
    if !owned_marker(&marker, data, pid)
        || !executable.metadata().is_ok_and(|entry| plain_file(&entry))
    {
        return Err(INVALID.into());
    }
    delete_file(executable).map_err(|_| CLEANUP_ERROR)?;
    delete_file(marker).map_err(|_| CLEANUP_ERROR)?;
    match delete_file(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == ERROR_DIR_NOT_EMPTY.to_hresult() => Ok(()),
        Err(_) => Err(CLEANUP_ERROR.into()),
    }
}

fn delete_file(file: File) -> windows::core::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // Mark only this opened and verified handle for removal when it closes.
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle().cast()),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_expected_installer_at_a_direct_stage_with_matching_version() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Saved Render Bridge");
        fs::create_dir(&data).unwrap();
        let stage = root.path().join("render-bridge-update-123AbC");
        fs::create_dir(&stage).unwrap();
        let executable = stage.join(INSTALLER_NAME);
        fs::write(&executable, b"fixture").unwrap();
        let arguments = |version: &str| {
            [
                data.as_os_str().to_owned(),
                OsString::from("123"),
                OsString::from(version),
            ]
        };
        let request =
            UpdateRequest::parse_at(arguments("1.2.3"), &executable, root.path(), "1.2.3").unwrap();
        assert_eq!(request.data_dir, data);
        assert_eq!(request.cleanup_stage, fs::canonicalize(&stage).unwrap());
        assert!(
            UpdateRequest::parse_at(arguments("1.2.2"), &executable, root.path(), "1.2.3").is_err()
        );
        assert!(
            UpdateRequest::parse_at(arguments("1.2.3"), &executable, data.as_path(), "1.2.3")
                .is_err()
        );
        let wrong_name = stage.join("other.exe");
        fs::write(&wrong_name, b"fixture").unwrap();
        assert!(
            UpdateRequest::parse_at(arguments("1.2.3"), &wrong_name, root.path(), "1.2.3").is_err()
        );
    }

    #[test]
    fn cleanup_removes_only_its_owned_stage_files() {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("render-bridge-update-A1B2C3");
        let marker = stage.join(HANDOFF_MARKER);
        let executable = stage.join(INSTALLER_NAME);
        let personal = stage.join("unrelated.txt");
        fs::create_dir(&stage).unwrap();
        fs::write(&executable, b"installer fixture").unwrap();
        fs::write(&personal, b"keep me").unwrap();
        assert!(clean_stage(&stage, root.path(), root.path(), 123).is_err());
        assert_eq!(fs::read(&executable).unwrap(), b"installer fixture");
        fs::write(&marker, marker_data(root.path(), 123).unwrap()).unwrap();
        assert!(clean_stage(&stage, root.path(), root.path(), 456).is_err());
        clean_stage(&stage, root.path(), root.path(), 123).unwrap();
        assert!(!marker.exists());
        assert!(!executable.exists());
        assert_eq!(fs::read(personal).unwrap(), b"keep me");

        fs::create_dir(root.path().join("render-bridge-update-Finished123")).unwrap();
        let stage = root.path().join("render-bridge-update-Finished123");
        fs::write(stage.join(INSTALLER_NAME), b"installer fixture").unwrap();
        fs::write(
            stage.join(HANDOFF_MARKER),
            marker_data(root.path(), 123).unwrap(),
        )
        .unwrap();
        clean_stage(&stage, root.path(), root.path(), 123).unwrap();
        assert!(!stage.exists());
    }

    #[test]
    fn process_wait_checks_path_and_has_a_deadline() {
        let running = std::env::current_exe().unwrap();
        let pid = std::process::id();
        let wrong = running.with_file_name("different-process-fixture.exe");
        let mismatch = wait_for_image(pid, &wrong, 1).unwrap_err();
        assert!(mismatch.contains("could not be verified"));
        let timeout = wait_for_image(pid, &running, 1).unwrap_err();
        assert!(timeout.contains("still closing"));
    }
}
