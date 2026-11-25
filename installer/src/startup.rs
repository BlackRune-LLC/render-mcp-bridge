use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{GENERIC_READ, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FileDispositionInfo, SetFileInformationByHandle,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistStream};
use windows::Win32::UI::Shell::{
    CommandLineToArgvW, FOLDERID_Startup, IShellLinkW, SHCreateMemStream, SLGP_RAWPATH, ShellLink,
};
use windows::core::{HSTRING, Interface};

use crate::shortcuts::{ComApartment, windows_argument, windows_known_folder, write_windows_link};

const LINK_NAME: &str = "Render Bridge.lnk";
const MAX_LINK_BYTES: u64 = 128 * 1024;
const READ_ERROR: &str = "Windows could not read the Render Bridge startup shortcut.";
const REMOVE_ERROR: &str = "Windows could not remove the Render Bridge startup shortcut.";
const CONFLICT: &str =
    "A different shortcut named Render Bridge already exists in the Windows Startup folder.";

pub fn enabled(executable: &Path, arguments: &[OsString]) -> Result<bool, String> {
    let path = startup_link()?;
    owned_entry(&path, executable, arguments, false).map(|entry| entry.is_some())
}

pub fn set_enabled(executable: &Path, arguments: &[OsString], enable: bool) -> Result<(), String> {
    set_enabled_at(&startup_link()?, executable, arguments, enable)
}

pub(crate) fn matches_existing_link(
    path: &Path,
    executable: &Path,
    arguments: &[OsString],
) -> Result<bool, String> {
    owned_entry(path, executable, arguments, false)
        .map(|entry| entry.is_some())
        .map_err(|_| "An existing Render Bridge app-menu shortcut could not be verified. Your shortcut was kept.".into())
}

pub(crate) fn remove_matching_link(
    path: &Path,
    executable: &Path,
    arguments: &[OsString],
) -> Result<(), String> {
    match owned_entry(path, executable, arguments, true) {
        Ok(Some(file)) => delete_link(file),
        Ok(None) => Ok(()),
        Err(error) if error == CONFLICT => Ok(()),
        Err(error) => Err(error),
    }
}

fn startup_link() -> Result<PathBuf, String> {
    let folder = windows_known_folder(
        &FOLDERID_Startup,
        "The current user's Windows Startup folder could not be located.",
    )?;
    Ok(folder.join(LINK_NAME))
}

fn set_enabled_at(
    path: &Path,
    executable: &Path,
    arguments: &[OsString],
    enable: bool,
) -> Result<(), String> {
    match (enable, owned_entry(path, executable, arguments, !enable)?) {
        (true, None) => {
            let directory = executable
                .parent()
                .filter(|_| executable.is_absolute() && executable.is_file())
                .ok_or("The Render Bridge executable could not be located.")?;
            let command = arguments
                .iter()
                .map(|argument| {
                    argument
                        .to_str()
                        .filter(|value| !value.contains('\0'))
                        .map(windows_argument)
                        .ok_or("The Render Bridge startup argument cannot be used on Windows.")
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(" ");
            let result = write_windows_link(
                path,
                executable,
                directory,
                &command,
                "Start Render Bridge hidden in the notification area",
            );
            if let Err(error) = result {
                // A second Render Bridge window may have created the same link.
                if owned_entry(path, executable, arguments, false)?.is_none() {
                    return Err(error);
                }
            }
            Ok(())
        }
        (false, Some(file)) => delete_link(file),
        _ => Ok(()),
    }
}

fn delete_link(file: File) -> Result<(), String> {
    delete_by_handle(&file).map_err(|_| REMOVE_ERROR.to_owned())
}

pub(crate) fn delete_by_handle(file: &File) -> windows::core::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // A rename or replacement of the path cannot change the open target.
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
}

fn owned_entry(
    path: &Path,
    executable: &Path,
    arguments: &[OsString],
    for_removal: bool,
) -> Result<Option<File>, String> {
    let access = GENERIC_READ.0 | if for_removal { DELETE.0 } else { 0 };
    let mut file = match OpenOptions::new()
        .read(true)
        .access_mode(access)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(READ_ERROR.into()),
    };
    let metadata = file.metadata().map_err(|_| READ_ERROR)?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(CONFLICT.into());
    }

    let mut data = Vec::new();
    (&mut file)
        .take(MAX_LINK_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| READ_ERROR)?;
    if data.len() as u64 > MAX_LINK_BYTES || !matches_link(&data, executable, arguments)? {
        return Err(CONFLICT.into());
    }
    Ok(Some(file))
}

fn matches_link(
    data: &[u8],
    executable: &Path,
    expected_args: &[OsString],
) -> Result<bool, String> {
    let _apartment = ComApartment::initialize()?;
    let mut target = vec![0; 32_768];
    let mut arguments = vec![0; 32_768];
    // Parse only the verified local file's bounded bytes. Do not resolve or run
    // the shortcut. GetPath with SLGP_RAWPATH reads the stored target.
    unsafe {
        let stream = SHCreateMemStream(Some(data)).ok_or(READ_ERROR)?;
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|_| READ_ERROR)?;
        let persistence = link.cast::<IPersistStream>().map_err(|_| READ_ERROR)?;
        if persistence.Load(&stream).is_err()
            || link
                .GetPath(&mut target, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
                .is_err()
            || link.GetArguments(&mut arguments).is_err()
        {
            return Ok(false);
        }
    }
    let target = PathBuf::from(decode_field(&target));
    // Check the path itself, so an unrelated link to another copy or alias is
    // never claimed. Windows Shell Link can normalize the casing of its path.
    let target = target.to_str();
    let executable = executable.to_str();
    let same_target = matches!((target, executable), (Some(target), Some(executable))
        if target.replace('/', "\\").eq_ignore_ascii_case(&executable.replace('/', "\\")));
    Ok(same_target && parse_arguments(&decode_field(&arguments)).as_deref() == Some(expected_args))
}

fn decode_field(value: &[u16]) -> OsString {
    OsString::from_wide(value.split(|unit| *unit == 0).next().unwrap_or_default())
}

fn parse_arguments(arguments: &OsStr) -> Option<Vec<OsString>> {
    let mut command_line = OsString::from("render-bridge ");
    command_line.push(arguments);
    let mut count = 0;
    // The Shell parser treats argv[0] differently. Prepend a fixed dummy
    // executable so it parses all stored arguments in their ordinary form.
    let parsed = unsafe { CommandLineToArgvW(&HSTRING::from(&command_line), &mut count) };
    if parsed.is_null() {
        return None;
    }
    unsafe {
        let arguments = (count >= 1).then(|| {
            let parsed = std::slice::from_raw_parts(parsed, count as usize);
            parsed[1..]
                .iter()
                .map(|argument| OsString::from_wide(argument.as_wide()))
                .collect()
        });
        let _ = LocalFree(Some(HLOCAL(parsed.cast())));
        arguments
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn arguments(url: &str) -> Vec<OsString> {
        ["--job-monitor", "--comfy-url", url, "--start-in-tray"]
            .into_iter()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn startup_entry_is_idempotent_and_removes_only_the_verified_file() {
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("Render Bridge 试.exe");
        let path = fixture.path().join(LINK_NAME);
        fs::write(&target, []).unwrap();
        let args = arguments("http://127.0.0.1:8188");
        assert!(owned_entry(&path, &target, &args, false).unwrap().is_none());
        set_enabled_at(&path, &target, &args, true).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(owned_entry(&path, &target, &args, false).unwrap().is_some());
        set_enabled_at(&path, &target, &args, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);

        let other = arguments("http://127.0.0.1:8189");
        assert_eq!(
            set_enabled_at(&path, &target, &other, true).unwrap_err(),
            CONFLICT
        );
        assert_eq!(
            set_enabled_at(&path, &target, &other, false).unwrap_err(),
            CONFLICT
        );
        remove_matching_link(&path, &target, &other).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);

        remove_matching_link(&path, &target, &args).unwrap();
        assert!(!path.exists());
        set_enabled_at(&path, &target, &args, false).unwrap();
    }

    #[test]
    fn recognizes_other_valid_quoting_without_accepting_unrelated_files() {
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("Render Bridge.exe");
        let path = fixture.path().join(LINK_NAME);
        fs::write(&target, []).unwrap();
        let args = arguments("http://127.0.0.1:8188");
        write_windows_link(
            &path,
            &target,
            fixture.path(),
            "--job-monitor --comfy-url http://127.0.0.1:8188 --start-in-tray",
            "Render Bridge",
        )
        .unwrap();
        assert!(owned_entry(&path, &target, &args, false).unwrap().is_some());
        fs::write(&path, b"unrelated").unwrap();
        assert_eq!(
            set_enabled_at(&path, &target, &args, false).unwrap_err(),
            CONFLICT
        );
        assert_eq!(fs::read(&path).unwrap(), b"unrelated");
    }
}
