use crate::setup::{Installed, venv_python};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

#[cfg(windows)]
const MONITOR_LINK_NAME: &str = "Render Bridge.lnk";
#[cfg(windows)]
const MONITOR_DIR: &str = "monitor";
#[cfg(windows)]
const MONITOR_EXECUTABLE: &str = "RenderBridgeMonitor.exe";
#[cfg(windows)]
const MONITOR_OWNER: &str = "render-bridge-monitor.owner";
#[cfg(windows)]
const MONITOR_OWNER_CONTENTS: &[u8] = b"Render Bridge monitor directory v1\n";
#[cfg(windows)]
const MONITOR_CONFLICT: &str = "Existing files in the Render Bridge monitor location could not be verified. The files were kept.";

pub fn create_gaming_shortcut(installed: &Installed) -> Result<PathBuf, String> {
    let app_dir = &installed.app_dir;
    let data_dir = &installed.data_dir;
    let python = venv_python(app_dir);
    let launcher = app_dir.join("setup_render_bridge.py");
    if !python.is_file() || !launcher.is_file() {
        return Err(
            "Finish installing Render Bridge before creating its Gaming Mode shortcut.".into(),
        );
    }
    let source = std::env::current_exe()
        .map_err(|_| "The running Render Bridge app could not be located.")?;
    #[cfg(windows)]
    {
        let executable = install_monitor_from(data_dir, &source)?;
        create_native_shortcut(
            data_dir,
            &[
                path_text(&executable)?,
                "--job-monitor",
                "--data-dir",
                path_text(data_dir)?,
            ],
        )
    }
    #[cfg(not(windows))]
    {
        stage_monitor(app_dir, data_dir, &source, create_native_shortcut)
    }
}

#[cfg(any(not(windows), test))]
fn stage_monitor(
    app_dir: &Path,
    data_dir: &Path,
    source: &Path,
    create_shortcut: impl FnOnce(&Path, &[&str]) -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    let state = path_text(data_dir)?;
    let stage_error = "The Render Bridge controls could not be copied into this installation.";
    let staging = tempfile::Builder::new()
        .prefix("controls-")
        .tempdir_in(app_dir)
        .map_err(|_| stage_error)?;
    let executable = staging.path().join(format!(
        "render-bridge-controls{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(source, &executable).map_err(|_| stage_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| stage_error)?;
    }
    let command = [
        path_text(&executable)?,
        "--job-monitor",
        "--data-dir",
        state,
    ];
    let shortcut = create_shortcut(app_dir, &command)?;
    // A saved shortcut owns this copy. Failure leaves the previous shortcut
    // untouched and drops the newly staged directory.
    let _ = staging.keep();
    Ok(shortcut)
}

#[cfg(windows)]
pub fn create_monitor_shortcut(installed: &Installed) -> Result<PathBuf, String> {
    use windows::Win32::UI::Shell::FOLDERID_Programs;

    let programs = windows_known_folder(
        &FOLDERID_Programs,
        "The current user's Windows app menu could not be located.",
    )?;
    let source = std::env::current_exe()
        .map_err(|_| "The running Render Bridge app could not be located.")?;
    let executable = install_monitor_from(&installed.data_dir, &source)?;
    app_menu_link_at(&programs, &installed.data_dir, &executable)
}

#[cfg(windows)]
pub fn open_monitor(installed: &Installed) -> Result<(), String> {
    let executable = install_current_monitor(&installed.data_dir)?;
    crate::setup::quiet_command(&executable)
        .args(["--job-monitor", "--data-dir"])
        .arg(&installed.data_dir)
        .current_dir(&installed.data_dir)
        .spawn()
        .map_err(|_| "Could not open Render Bridge. Try again from setup.".to_owned())?;
    Ok(())
}

#[cfg(windows)]
pub(crate) fn install_current_monitor(data: &Path) -> Result<PathBuf, String> {
    let source = std::env::current_exe()
        .map_err(|_| "The running Render Bridge app could not be located.")?;
    install_monitor_from(data, &source)
}

#[cfg(windows)]
pub(crate) fn existing_monitor_executable(data: &Path) -> Result<PathBuf, String> {
    if !data.is_absolute() {
        return Err(MONITOR_CONFLICT.into());
    }
    let directory = data.join(MONITOR_DIR);
    verify_monitor_directory(&directory)?;
    let executable = directory.join(MONITOR_EXECUTABLE);
    let metadata = std::fs::symlink_metadata(&executable).map_err(|_| MONITOR_CONFLICT)?;
    if !metadata.is_file() || is_reparse(&metadata) {
        return Err(MONITOR_CONFLICT.into());
    }
    Ok(executable)
}

#[cfg(windows)]
fn app_menu_link_at(programs: &Path, data: &Path, executable: &Path) -> Result<PathBuf, String> {
    let destination = programs.join(MONITOR_LINK_NAME);
    let command = ["--job-monitor", "--data-dir", path_text(data)?];
    let expected = command.map(std::ffi::OsString::from);
    if crate::startup::matches_existing_link(&destination, executable, &expected)? {
        return Ok(destination);
    }
    let arguments = command.map(windows_argument).join(" ");
    let written = write_windows_link(
        &destination,
        executable,
        data,
        &arguments,
        "Open Render Bridge",
    );
    if written.is_err()
        && crate::startup::matches_existing_link(&destination, executable, &expected)?
    {
        // Another setup can finish the same per-user link while this one stages it.
        return Ok(destination);
    }
    written
}

#[cfg(windows)]
fn install_monitor_from(data: &Path, source: &Path) -> Result<PathBuf, String> {
    use std::fs;

    const PREPARE: &str = "Could not prepare the Render Bridge monitor folder.";
    const WRITE: &str = "Could not install the Render Bridge monitor.";
    if !data.is_absolute() || !data.is_dir() {
        return Err(PREPARE.into());
    }
    let directory = data.join(MONITOR_DIR);
    match fs::symlink_metadata(&directory) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let staged = tempfile::tempdir_in(data).map_err(|_| PREPARE)?;
            fs::write(staged.path().join(MONITOR_OWNER), MONITOR_OWNER_CONTENTS)
                .map_err(|_| PREPARE)?;
            if fs::rename(staged.path(), &directory).is_err() && !directory.exists() {
                return Err(PREPARE.into());
            }
        }
        Err(_) => return Err(PREPARE.into()),
    }
    verify_monitor_directory(&directory)?;

    let executable = directory.join(MONITOR_EXECUTABLE);
    let mut setup = fs::File::open(source).map_err(|_| WRITE)?;
    let exists = match fs::symlink_metadata(&executable) {
        Ok(metadata) if metadata.is_file() && !is_reparse(&metadata) => {
            if same_contents(&mut setup, &executable, metadata.len()).map_err(|_| WRITE)? {
                return Ok(executable);
            }
            true
        }
        Ok(_) => return Err(MONITOR_CONFLICT.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(_) => return Err(WRITE.into()),
    };
    let mut staged = NamedTempFile::new_in(directory).map_err(|_| WRITE)?;
    io::copy(&mut setup, staged.as_file_mut()).map_err(|_| WRITE)?;
    staged.as_file().sync_all().map_err(|_| WRITE)?;
    if exists {
        staged.persist(&executable).map_err(|error| {
            if matches!(error.error.raw_os_error(), Some(5 | 32 | 33)) {
                "Exit Render Bridge from the notification area, then retry the monitor update."
            } else {
                WRITE
            }
        })?;
    } else {
        staged.persist_noclobber(&executable).map_err(|_| WRITE)?;
    }
    Ok(executable)
}

#[cfg(windows)]
fn verify_monitor_directory(directory: &Path) -> Result<(), String> {
    use std::fs;

    let metadata = fs::symlink_metadata(directory).map_err(|_| MONITOR_CONFLICT)?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err(MONITOR_CONFLICT.into());
    }
    let owner = directory.join(MONITOR_OWNER);
    let marker = fs::symlink_metadata(&owner).map_err(|_| MONITOR_CONFLICT)?;
    if !marker.is_file()
        || is_reparse(&marker)
        || marker.len() != MONITOR_OWNER_CONTENTS.len() as u64
        || fs::read(owner).map_err(|_| MONITOR_CONFLICT)? != MONITOR_OWNER_CONTENTS
    {
        return Err(MONITOR_CONFLICT.into());
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse(entry: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    entry.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

#[cfg(windows)]
fn same_contents(setup: &mut std::fs::File, executable: &Path, length: u64) -> io::Result<bool> {
    use std::io::{Read, Seek};

    if setup.metadata()?.len() != length {
        return Ok(false);
    }
    let mut current = std::fs::File::open(executable)?;
    let mut original = [0; 8192];
    let mut installed = [0; 8192];
    let mut remaining = length;
    while remaining != 0 {
        let count = remaining.min(original.len() as u64) as usize;
        setup.read_exact(&mut original[..count])?;
        current.read_exact(&mut installed[..count])?;
        if original[..count] != installed[..count] {
            setup.rewind()?;
            return Ok(false);
        }
        remaining -= count as u64;
    }
    Ok(true)
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "Render Bridge shortcuts require Unicode installation paths.".into())
}

fn save_shortcut(destination: &Path, contents: &[u8], executable: bool) -> Result<PathBuf, String> {
    let create_error = || {
        format!(
            "Render Bridge shortcut could not be saved to {}.",
            destination.display()
        )
    };
    let directory = destination.parent().ok_or_else(create_error)?;
    let mut file = NamedTempFile::new_in(directory).map_err(|_| create_error())?;
    file.write_all(contents).map_err(|_| create_error())?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|_| create_error())?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    file.persist_noclobber(destination).map_err(|error| {
        if error.error.kind() == io::ErrorKind::AlreadyExists {
            format!(
                "A shortcut already exists at {}. Rename or remove it to create another.",
                destination.display()
            )
        } else {
            create_error()
        }
    })?;
    Ok(destination.to_owned())
}

#[cfg(windows)]
pub(crate) struct ComApartment(bool);

#[cfg(windows)]
impl ComApartment {
    pub(crate) fn initialize() -> Result<Self, String> {
        use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

        // An existing apartment remains usable with its original threading model.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self(false));
        }
        result
            .ok()
            .map_err(|_| "Windows shortcuts are unavailable.")?;
        Ok(Self(true))
    }
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // This guard stays on the thread that initialized the apartment.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

#[cfg(windows)]
pub(crate) fn windows_known_folder(
    id: &windows::core::GUID,
    error: &str,
) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    // The known-folder API allocates a NUL-terminated path. Copy it before freeing it.
    let directory = unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).map_err(|_| error.to_owned())?;
        let directory = PathBuf::from(std::ffi::OsString::from_wide(path.as_wide()));
        CoTaskMemFree(Some(path.0.cast()));
        directory
    };
    if !directory.is_dir() {
        return Err(error.into());
    }
    Ok(directory)
}

#[cfg(windows)]
fn create_native_shortcut(app_dir: &Path, command: &[&str]) -> Result<PathBuf, String> {
    use windows::Win32::UI::Shell::FOLDERID_Desktop;

    let desktop = windows_known_folder(
        &FOLDERID_Desktop,
        "The Windows Desktop folder could not be located.",
    )?;
    write_windows_shortcut(&desktop, app_dir, command)
}

#[cfg(windows)]
fn write_windows_shortcut(
    desktop: &Path,
    app_dir: &Path,
    command: &[&str],
) -> Result<PathBuf, String> {
    let arguments = command[1..]
        .iter()
        .map(|argument| windows_argument(argument))
        .collect::<Vec<_>>()
        .join(" ");
    write_windows_link(
        &desktop.join("Render Bridge Gaming Mode.lnk"),
        Path::new(command[0]),
        app_dir,
        &arguments,
        "Check ComfyUI jobs and control Render Bridge Gaming Mode",
    )
}

#[cfg(windows)]
pub(crate) fn write_windows_link(
    destination: &Path,
    executable: &Path,
    working_directory: &Path,
    arguments: &str,
    description: &str,
) -> Result<PathBuf, String> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};

    let _apartment = ComApartment::initialize()?;
    let save_error = "Windows could not create the Render Bridge shortcut.";
    let directory = destination.parent().ok_or(save_error)?;
    let staging = tempfile::tempdir_in(directory).map_err(|_| save_error)?;
    let shortcut = staging.path().join("Render Bridge.lnk");
    let native = |error: windows::core::Error| format!("{save_error} ({})", error.code());
    // This thread owns a COM apartment. Each HSTRING is NUL-terminated.
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(native)?;
        link.SetPath(&HSTRING::from(executable.as_os_str()))
            .map_err(native)?;
        link.SetArguments(&HSTRING::from(arguments))
            .map_err(native)?;
        link.SetWorkingDirectory(&HSTRING::from(working_directory.as_os_str()))
            .map_err(native)?;
        link.SetDescription(&HSTRING::from(description))
            .map_err(native)?;
        link.cast::<IPersistFile>()
            .map_err(native)?
            .Save(&HSTRING::from(shortcut.as_os_str()), true)
            .map_err(native)?;
    };
    let contents = std::fs::read(shortcut).map_err(|_| save_error)?;
    save_shortcut(destination, &contents, false)
}

#[cfg(any(windows, test))]
pub(crate) fn windows_argument(argument: &str) -> String {
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        let count = if character == '"' {
            slashes * 2 + 1
        } else {
            slashes
        };
        quoted.extend(std::iter::repeat_n('\\', count));
        quoted.push(character);
        slashes = 0;
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn user_directory(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(target_os = "macos")]
fn create_native_shortcut(_app_dir: &Path, command: &[&str]) -> Result<PathBuf, String> {
    let desktop = user_directory("HOME")
        .map(|path| path.join("Desktop"))
        .filter(|path| path.is_dir())
        .ok_or("The macOS Desktop folder could not be located.")?;
    let command = command
        .iter()
        .map(|argument| shell_argument(argument))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!("#!/bin/sh\nexec {command}\n");
    save_shortcut(
        &desktop.join("Render Bridge Gaming Mode.command"),
        script.as_bytes(),
        true,
    )
}

#[cfg(any(target_os = "macos", test))]
fn shell_argument(argument: &str) -> String {
    format!("'{}'", argument.replace('\'', "'\"'\"'"))
}

#[cfg(target_os = "linux")]
fn create_native_shortcut(_app_dir: &Path, command: &[&str]) -> Result<PathBuf, String> {
    let directory = user_directory("XDG_DATA_HOME")
        .or_else(|| user_directory("HOME").map(|path| path.join(".local/share")))
        .ok_or("The Linux applications folder could not be located.")?
        .join("applications");
    let command = command
        .iter()
        .map(|argument| desktop_argument(argument))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Render Bridge Gaming Mode\n\
         Comment=Check ComfyUI jobs and control Render Bridge Gaming Mode\nTerminal=false\nExec={command}\n\
         Categories=Utility;\n"
    );
    std::fs::create_dir_all(&directory)
        .map_err(|_| "The Linux applications folder could not be created.")?;
    save_shortcut(
        &directory.join("render-bridge-gaming-mode.desktop"),
        entry.as_bytes(),
        false,
    )
}

#[cfg(any(target_os = "linux", test))]
fn desktop_argument(argument: &str) -> Result<String, String> {
    if argument.chars().any(char::is_control) {
        return Err(
            "The installation path contains a control character that Linux shortcuts cannot use."
                .into(),
        );
    }
    let mut quoted = String::from("\"");
    for character in argument.chars() {
        match character {
            '\\' | '"' | '$' | '`' => {
                // Desktop entries decode string escapes before decoding the Exec argument.
                quoted.push_str("\\\\");
                if character == '\\' {
                    quoted.push('\\');
                }
                quoted.push(character);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn create_native_shortcut(_app_dir: &Path, _command: &[&str]) -> Result<PathBuf, String> {
    Err("Gaming Mode shortcuts are supported on Windows, macOS, and Linux.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_a_shortcut_preserves_any_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("Gaming Mode shortcut");
        assert_eq!(
            save_shortcut(&destination, b"original", false).unwrap(),
            destination
        );
        assert!(save_shortcut(&destination, b"replacement", false).is_err());
        assert_eq!(std::fs::read(destination).unwrap(), b"original");
    }

    #[test]
    fn staged_monitor_persists_only_if_its_shortcut_is_saved() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("installer");
        let data_dir = fixture.path().join("State with spaces");
        let app_dir = data_dir.join("app");
        let shortcut = fixture.path().join("shortcut");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(&source, b"installer bytes").unwrap();
        std::fs::write(&shortcut, b"existing shortcut").unwrap();

        let mut previous_copy = PathBuf::new();
        let existing = stage_monitor(&app_dir, &data_dir, &source, |directory, command| {
            assert_eq!(directory, app_dir);
            previous_copy = PathBuf::from(command[0]);
            assert_eq!(std::fs::read(&previous_copy).unwrap(), b"installer bytes");
            save_shortcut(&shortcut, b"would replace the existing shortcut", false)
        });
        assert!(existing.is_err());
        assert!(!previous_copy.exists());
        assert_eq!(std::fs::read(&shortcut).unwrap(), b"existing shortcut");

        let new_shortcut = fixture.path().join("new shortcut");
        let mut retained_copy = PathBuf::new();
        assert_eq!(
            stage_monitor(&app_dir, &data_dir, &source, |_, command| {
                retained_copy = PathBuf::from(command[0]);
                assert_eq!(
                    &command[1..],
                    &["--job-monitor", "--data-dir", path_text(&data_dir).unwrap()]
                );
                save_shortcut(&new_shortcut, command[0].as_bytes(), false)
            })
            .unwrap(),
            new_shortcut
        );
        assert!(retained_copy.starts_with(&app_dir));
        assert_eq!(std::fs::read(retained_copy).unwrap(), b"installer bytes");
    }

    #[cfg(windows)]
    #[test]
    fn public_monitor_has_one_stable_executable_and_an_idempotent_app_link() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("Render Bridge state");
        let programs = fixture.path().join("Programs");
        let source = fixture.path().join("setup.exe");
        std::fs::create_dir(&state).unwrap();
        std::fs::create_dir(&programs).unwrap();
        std::fs::write(&source, b"version 1").unwrap();

        let first = install_monitor_from(&state, &source).unwrap();
        let link = app_menu_link_at(&programs, &state, &first).unwrap();
        let original_link = std::fs::read(&link).unwrap();
        std::fs::write(&source, b"version 2").unwrap();
        assert_eq!(install_monitor_from(&state, &source).unwrap(), first);
        assert_eq!(std::fs::read(&first).unwrap(), b"version 2");
        assert_eq!(app_menu_link_at(&programs, &state, &first).unwrap(), link);
        assert_eq!(std::fs::read(link).unwrap(), original_link);
    }

    #[cfg(windows)]
    #[test]
    fn public_monitor_keeps_unowned_paths_and_reports_a_locked_update() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let source = fixture.path().join("setup.exe");
        std::fs::create_dir(&state).unwrap();
        std::fs::write(&source, b"first version").unwrap();
        let executable = install_monitor_from(&state, &source).unwrap();
        let unrelated_link = fixture.path().join(MONITOR_LINK_NAME);
        std::fs::write(&unrelated_link, b"personal shortcut").unwrap();
        assert!(app_menu_link_at(fixture.path(), &state, &executable).is_err());
        assert_eq!(std::fs::read(unrelated_link).unwrap(), b"personal shortcut");

        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(&executable)
            .unwrap();
        std::fs::write(&source, b"updated version").unwrap();
        let error = install_monitor_from(&state, &source).unwrap_err();
        assert!(error.contains("Exit Render Bridge"), "{error}");
        assert_eq!(std::fs::read(&executable).unwrap(), b"first version");
        drop(locked);

        std::fs::remove_file(state.join(MONITOR_DIR).join(MONITOR_OWNER)).unwrap();
        assert!(install_monitor_from(&state, &source).is_err());
        assert_eq!(std::fs::read(executable).unwrap(), b"first version");
    }

    #[test]
    fn windows_arguments_handle_spaces_quotes_and_trailing_slashes() {
        assert_eq!(
            windows_argument(r"C:\Render Bridge\根\"),
            r#""C:\Render Bridge\根\\""#
        );
        assert_eq!(
            windows_argument(r#"literal\"quote"#),
            r#""literal\\\"quote""#
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_shortcut_round_trips_target_and_arguments() {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        use windows::core::{HSTRING, Interface};

        let _apartment = ComApartment::initialize().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let desktop = fixture.path().join("Desktop 试");
        std::fs::create_dir(&desktop).unwrap();
        let data_dir = fixture.path().join("State 试 O'Brien & 100%");
        let app_dir = data_dir.join("app");
        let monitor = app_dir.join("render-bridge-controls.exe");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(&monitor, []).unwrap();
        let command = [
            path_text(&monitor).unwrap(),
            "--job-monitor",
            "--data-dir",
            path_text(&data_dir).unwrap(),
        ];
        let shortcut = write_windows_shortcut(&desktop, &app_dir, &command).unwrap();

        let decode = |value: &[u16]| {
            String::from_utf16(value.split(|unit| *unit == 0).next().unwrap()).unwrap()
        };
        let mut path = vec![0; 32_768];
        let mut arguments = vec![0; 32_768];
        let mut working_directory = vec![0; 32_768];
        // The local test only loads a Shell Link and reads its stored fields.
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.cast::<IPersistFile>()
                .unwrap()
                .Load(&HSTRING::from(shortcut.as_os_str()), STGM_READ)
                .unwrap();
            link.GetPath(&mut path, std::ptr::null_mut(), 0).unwrap();
            link.GetArguments(&mut arguments).unwrap();
            link.GetWorkingDirectory(&mut working_directory).unwrap();
        };
        assert_eq!(Path::new(&decode(&path)), monitor);
        assert_eq!(
            decode(&arguments),
            format!(
                "\"--job-monitor\" \"--data-dir\" \"{}\"",
                data_dir.display()
            )
        );
        assert_eq!(Path::new(&decode(&working_directory)), app_dir);
        let original = std::fs::read(&shortcut).unwrap();
        assert!(write_windows_shortcut(&desktop, &app_dir, &command).is_err());
        assert_eq!(std::fs::read(shortcut).unwrap(), original);
    }

    #[test]
    fn macos_arguments_quote_shell_metacharacters() {
        assert_eq!(
            shell_argument("/data/O'Brien $()"),
            "'/data/O'\"'\"'Brien $()'"
        );
    }

    #[test]
    fn desktop_arguments_escape_both_decoding_layers() {
        assert_eq!(
            desktop_argument(r#"/data/A $HOME `%f\""#).unwrap(),
            r#""/data/A \\$HOME \\`%%f\\\\\\"""#
        );
        assert!(desktop_argument("/data/line\nbreak").is_err());
    }
}
