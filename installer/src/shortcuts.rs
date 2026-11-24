use crate::setup::{Installed, venv_python};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

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
    let command = [
        path_text(&python)?,
        path_text(&launcher)?,
        "--data-dir",
        path_text(data_dir)?,
        "gaming-mode",
        "toggle",
        "--wait",
    ];
    create_native_shortcut(app_dir, &command)
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "Gaming Mode shortcuts require Unicode installation paths.".into())
}

fn save_shortcut(destination: &Path, contents: &[u8], executable: bool) -> Result<PathBuf, String> {
    let create_error = || {
        format!(
            "Gaming Mode shortcut could not be saved to {}.",
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
struct ComApartment(bool);

#[cfg(windows)]
impl ComApartment {
    fn initialize() -> Result<Self, String> {
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
fn create_native_shortcut(app_dir: &Path, command: &[&str]) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    let _apartment = ComApartment::initialize()?;
    // The known-folder API allocates a NUL-terminated path. Copy it before freeing it.
    let desktop = unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None)
            .map_err(|_| "The Windows Desktop folder could not be located.")?;
        let desktop = PathBuf::from(std::ffi::OsString::from_wide(path.as_wide()));
        CoTaskMemFree(Some(path.0.cast()));
        desktop
    };
    if !desktop.is_dir() {
        return Err("The Windows Desktop folder could not be located.".into());
    }
    write_windows_shortcut(&desktop, app_dir, command)
}

#[cfg(windows)]
fn write_windows_shortcut(
    desktop: &Path,
    app_dir: &Path,
    command: &[&str],
) -> Result<PathBuf, String> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface, w};

    let save_error = "Windows could not create the Gaming Mode shortcut.";
    let staging = tempfile::tempdir_in(desktop).map_err(|_| save_error)?;
    let shortcut = staging.path().join("Gaming Mode.lnk");
    let arguments = command[1..]
        .iter()
        .map(|argument| windows_argument(argument))
        .collect::<Vec<_>>()
        .join(" ");
    let native = |error: windows::core::Error| format!("{save_error} ({})", error.code());
    // The caller owns a COM apartment. Each HSTRING holds a NUL-terminated UTF-16 value.
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(native)?;
        link.SetPath(&HSTRING::from(command[0].replace('/', "\\")))
            .map_err(native)?;
        link.SetArguments(&HSTRING::from(arguments))
            .map_err(native)?;
        link.SetWorkingDirectory(&HSTRING::from(path_text(app_dir)?.replace('/', "\\")))
            .map_err(native)?;
        link.SetDescription(w!("Toggle Gaming Mode for Render Bridge"))
            .map_err(native)?;
        link.cast::<IPersistFile>()
            .map_err(native)?
            .Save(&HSTRING::from(shortcut.as_os_str()), true)
            .map_err(native)?;
    };
    let contents = std::fs::read(shortcut).map_err(|_| save_error)?;
    save_shortcut(
        &desktop.join("Render Bridge Gaming Mode.lnk"),
        &contents,
        false,
    )
}

#[cfg(any(windows, test))]
fn windows_argument(argument: &str) -> String {
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
         Comment=Toggle Gaming Mode for Render Bridge\nTerminal=true\nExec={command}\n\
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
        let python = venv_python(&app_dir);
        let launcher = app_dir.join("setup_render_bridge.py");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, []).unwrap();
        std::fs::write(&launcher, []).unwrap();
        let command = [
            path_text(&python).unwrap(),
            path_text(&launcher).unwrap(),
            "--data-dir",
            path_text(&data_dir).unwrap(),
            "gaming-mode",
            "toggle",
            "--wait",
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
        assert_eq!(Path::new(&decode(&path)), python);
        assert_eq!(
            decode(&arguments),
            format!(
                "\"{}\" \"--data-dir\" \"{}\" \"gaming-mode\" \"toggle\" \"--wait\"",
                launcher.display(),
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
