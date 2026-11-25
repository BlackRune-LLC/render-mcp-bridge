use std::fs::{self, File};
use std::io::{self, Read};
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf, Prefix};

use base64::Engine;
use serde::Deserialize;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

use crate::setup::{Installed, lock_install, quiet_command, validate_comfy_url};
use crate::uninstall::pin_local_directories;

const MANIFEST: &str = "attached-mcp.json";
const LAUNCHER: &str = "start_mcp_server.bat";
const SCRIPT: &str = include_str!("attached_mcp.ps1");
const INVALID: &str = "The attached MCP controller configuration is missing or invalid.";

#[derive(Clone, Copy)]
pub enum Action {
    Start,
    Stop,
    AutoStart,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    port: u16,
    launcher: PathBuf,
}

fn selected_port(endpoint: &str) -> Option<u16> {
    let origin = endpoint.strip_suffix("/mcp")?;
    let origin = validate_comfy_url(origin).ok()?;
    url::Url::parse(&origin).ok()?.port_or_known_default()
}

fn parse_manifest(bytes: &[u8], endpoint: &str) -> Option<Registration> {
    let manifest: Registration = serde_json::from_slice(bytes).ok()?;
    let path = &manifest.launcher;
    let mut components = path.components();
    let local = matches!(
        components.next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
    );
    let regular_components =
        components.all(|part| matches!(part, Component::RootDir | Component::Normal(_)));
    let name_matches = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(LAUNCHER));
    (manifest.port != 0
        && Some(manifest.port) == selected_port(endpoint)
        && path.is_absolute()
        && local
        && regular_components
        && name_matches)
        .then_some(manifest)
}

fn regular_file(path: &Path) -> io::Result<bool> {
    match path.symlink_metadata() {
        Ok(meta)
            if meta.is_file() && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0 =>
        {
            Ok(true)
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(crate) fn registration(installed: &Installed) -> Result<Registration, String> {
    const MAX_BYTES: usize = 8192;
    let path = installed.data_dir.join(MANIFEST);
    if !installed.attach_only || !matches!(regular_file(&path), Ok(true)) {
        return Err(INVALID.into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes))
        .map_err(|_| INVALID)?;
    if bytes.len() > MAX_BYTES {
        return Err(INVALID.into());
    }
    let manifest = parse_manifest(
        bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes),
        &installed.mcp_url,
    )
    .ok_or(INVALID)?;
    if !matches!(regular_file(&manifest.launcher), Ok(true)) {
        return Err("The configured attached MCP launcher is unavailable.".into());
    }
    let comfy_port = crate::setup::installed_comfy_url(installed)
        .and_then(|url| url::Url::parse(&url).ok())
        .and_then(|url| url.port_or_known_default())
        .ok_or(INVALID)?;
    if comfy_port == manifest.port {
        return Err("Choose a different port for the attached MCP server and ComfyUI.".into());
    }
    Ok(manifest)
}

pub fn configured(installed: &Installed) -> bool {
    registration(installed).is_ok()
}

fn stopped_marker(data: &Path, port: u16) -> PathBuf {
    data.join(format!("attached-mcp-{port}.stopped"))
}

pub(crate) fn control(
    installed: &Installed,
    action: Action,
    preflight: impl FnOnce(&Installed, &Registration) -> Result<(), String>,
) -> Result<(), String> {
    let _data_pins = pin_local_directories(&installed.data_dir)?;
    let _install_lock = lock_install(&installed.data_dir)?;
    let current = crate::setup::existing_install_at(&installed.data_dir)
        .filter(|current| {
            current.mcp_url == installed.mcp_url && current.app_dir == installed.app_dir
        })
        .ok_or("The Render Bridge installation changed. Reopen the panel and try again.")?;
    let manifest = registration(&current)?;
    let root = manifest.launcher.parent().ok_or(INVALID)?;
    let _launcher_pins = pin_local_directories(root)?;
    preflight(&current, &manifest)?;
    with_stop_setting(&installed.data_dir, manifest.port, action, || {
        run_controller(&manifest, action)
    })
}

fn with_stop_setting(
    data: &Path,
    port: u16,
    action: Action,
    run: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let marker = stopped_marker(data, port);
    let stopped =
        regular_file(&marker).map_err(|_| "Could not read the attached MCP stop setting.")?;
    if matches!(action, Action::AutoStart) && stopped {
        return Ok(());
    }

    let new_marker = matches!(action, Action::Stop) && !stopped;
    if new_marker {
        File::create_new(&marker).map_err(|_| "Could not save the attached MCP stop setting.")?;
    }
    if let Err(error) = run() {
        if new_marker {
            fs::remove_file(&marker)
                .map_err(|_| format!("{error} The sign-in stop setting could not be restored."))?;
        }
        return Err(error);
    }

    if matches!(action, Action::Start) && stopped {
        fs::remove_file(&marker).map_err(
            |_| "The MCP server started, but the sign-in stop setting could not be cleared.",
        )?;
    }
    Ok(())
}

fn powershell(script: &str) -> Result<std::process::Command, String> {
    let system_root = std::env::var_os("SystemRoot").ok_or("Windows PowerShell is unavailable.")?;
    let program = PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if !program.is_file() {
        return Err("Windows PowerShell is unavailable.".into());
    }
    let script = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    let mut command = quiet_command(program);
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
        ])
        .arg(encoded);
    Ok(command)
}

fn run_controller(manifest: &Registration, action: Action) -> Result<(), String> {
    let operation = match action {
        Action::Start | Action::AutoStart => "start",
        Action::Stop => "stop",
    };
    let status = powershell(SCRIPT)?
        .env("RB_ATTACHED_PORT", manifest.port.to_string())
        .env("RB_ATTACHED_LAUNCHER", &manifest.launcher)
        .env("RB_ATTACHED_OPERATION", operation)
        .status()
        .map_err(|_| "Could not run the attached MCP controller.")?;
    match status.code() {
        Some(0) => Ok(()),
        Some(2) => Err("The configured MCP port is occupied by an unverified process.".into()),
        Some(3) => Err("The configured MCP launcher did not start its listener.".into()),
        Some(4) => Err("The verified MCP listener could not be stopped safely.".into()),
        _ => Err("Could not verify or control the attached MCP server.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(port: u16, launcher: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({ "port": port, "launcher": launcher })).unwrap()
    }

    #[test]
    fn attached_controller_accepts_only_the_installations_loopback_mcp_port() {
        let launcher = r"C:\Render Files\Private\start_mcp_server.bat";
        let config = manifest(19379, launcher);
        assert!(parse_manifest(&config, "http://127.0.0.1:19379/mcp").is_some());
        for endpoint in [
            "http://127.0.0.1:19380/mcp",
            "http://example.test:19379/mcp",
            "http://user@localhost:19379/mcp",
            "http://127.0.0.1:19379/something-else",
        ] {
            assert!(parse_manifest(&config, endpoint).is_none(), "{endpoint}");
        }
        for path in [
            r"start_mcp_server.bat",
            r"C:\Other\unrelated.bat",
            r"C:\Root\..\start_mcp_server.bat",
            r"\\network\share\start_mcp_server.bat",
        ] {
            assert!(
                parse_manifest(&manifest(19379, path), "http://localhost:19379/mcp").is_none(),
                "{path}"
            );
        }
    }

    #[test]
    fn stop_settings_are_independent_for_different_ports() {
        let folder = Path::new(r"C:\Render Files");
        let first = stopped_marker(folder, 19379);
        let other = stopped_marker(folder, 19380);
        assert_eq!(first, folder.join("attached-mcp-19379.stopped"));
        assert_ne!(first, other);
    }

    #[test]
    fn sign_in_respects_a_persisted_stop_until_an_explicit_start_succeeds() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path();
        let marker = stopped_marker(data, 19379);
        assert!(with_stop_setting(data, 19379, Action::Stop, || Err("foreign".into())).is_err());
        assert!(!marker.exists());
        with_stop_setting(data, 19379, Action::Stop, || Ok(())).unwrap();
        assert!(marker.is_file());
        with_stop_setting(data, 19379, Action::AutoStart, || {
            panic!("must remain stopped")
        })
        .unwrap();
        assert!(with_stop_setting(data, 19379, Action::Start, || Err("failed".into())).is_err());
        assert!(marker.is_file());
        with_stop_setting(data, 19380, Action::AutoStart, || Ok(())).unwrap();
        with_stop_setting(data, 19379, Action::Start, || Ok(())).unwrap();
        assert!(!marker.exists());
    }

    #[test]
    fn embedded_powershell_fits_the_windows_command_line() {
        let utf16_bytes = SCRIPT.encode_utf16().count() * 2;
        assert!(utf16_bytes.div_ceil(3) * 4 < 30_000);
    }
    #[test]
    fn owner_selection_requires_the_configured_bound_process_and_its_ancestry() {
        let script = SCRIPT.replace("\r\n", "\n");
        let definitions = script.split_once("\ntry {\n    $port = ").unwrap().0;
        let fixture = r#"
$launcher = 'C:\Private MCP\start_mcp_server.bat'
$shim = 'C:\Private MCP\.venv\Scripts\python.exe'
$source = 'C:\Private MCP\comfy_mcp_server.py'
$cmd = 'C:\Windows\System32\cmd.exe'
$known = @{
    3 = [pscustomobject]@{ ProcessId = 3; CreationDate = 40; ParentProcessId = 2; Name = 'python.exe'; CommandLine = 'python.exe -I comfy_mcp_server.py'; ExecutablePath = 'C:\Python\python.exe' }
    2 = [pscustomobject]@{ ProcessId = 2; CreationDate = 30; ParentProcessId = 1; Name = 'python.exe'; CommandLine = 'python.exe -I comfy_mcp_server.py'; ExecutablePath = $shim }
    1 = [pscustomobject]@{ ProcessId = 1; CreationDate = 20; Name = 'cmd.exe'; CommandLine = 'cmd.exe /d /c start_mcp_server.bat'; ExecutablePath = $cmd }
}
$lookup = { param($id) $known[$id] }.GetNewClosure()
$rows = @(
    [pscustomobject]@{ ProcessId = 3; Address = '127.0.0.1' },
    [pscustomobject]@{ ProcessId = 3; Address = '100.100.0.7' }
)
$owned = Select-Owned $rows $launcher $shim $source $cmd $lookup
if ($null -eq $owned -or $owned.Listener.ProcessId -ne 3 -or $owned.Wrapper.ProcessId -ne 1) { exit 11 }
$known[1].CommandLine = 'cmd.exe /c "C:\Private MCP\start_mcp_server.bat"'
$owned = Select-Owned $rows $launcher $shim $source $cmd $lookup
if ($null -eq $owned) { exit 12 }
if (Test-Same $owned.Listener ([pscustomobject]@{ProcessId=3; CreationDate=41})) { exit 13 }
$foreign = $rows + [pscustomobject]@{ ProcessId = 4; Address = '0.0.0.0' }
if ($null -ne (Select-Owned $foreign $launcher $shim $source $cmd $lookup)) { exit 14 }
$known[3].CommandLine = 'python.exe -I "D:\Unrelated\comfy_mcp_server.py"'
if ($null -ne (Select-Owned $rows $launcher $shim $source $cmd $lookup)) { exit 15 }
$known[3].CommandLine = 'python.exe -I comfy_mcp_server.py'
$known[1].CommandLine = 'cmd.exe /c "D:\Unrelated\start_mcp_server.bat"'
if ($null -ne (Select-Owned $rows $launcher $shim $source $cmd $lookup)) { exit 16 }
$known[1].CommandLine = 'cmd.exe /c "C:\Private MCP\start_mcp_server.bat"'
$known[2].ExecutablePath = 'D:\Unrelated\.venv\Scripts\python.exe'
if ($null -ne (Select-Owned $rows $launcher $shim $source $cmd $lookup)) { exit 17 }
$known[2].ExecutablePath = $shim
$known[2].CreationDate = 41
if ($null -ne (Select-Owned $rows $launcher $shim $source $cmd $lookup)) { exit 18 }
$known[2].CreationDate = 30
$known[1].CreationDate = 31
if ($null -ne (Select-Owned $rows $launcher $shim $source $cmd $lookup)) { exit 19 }
$known[1].CreationDate = 20

# Failure cleanup can find an exact Python child even before it owns a socket.
$wrongImage = $known[2].PSObject.Copy()
$wrongImage.ProcessId = 4
$wrongImage.ExecutablePath = 'D:\Unrelated\.venv\Scripts\python.exe'
$late = $known[2].PSObject.Copy()
$late.ProcessId = 5
$late.CreationDate = 36
$early = $known[2].PSObject.Copy()
$early.ProcessId = 6
$early.CreationDate = 19
$comfy = $known[3].PSObject.Copy()
$comfy.ProcessId = 7
$comfy.CommandLine = 'python.exe main.py'
$fakeChild = $known[3].PSObject.Copy()
$fakeChild.ProcessId = 8
$fakeChild.ParentProcessId = 22
$shimTargets = @(Select-LaunchedPython @($known[2], $wrongImage, $late, $early) $known[1] 35 $source $shim)
if ($shimTargets.Count -ne 1 -or $shimTargets[0].ProcessId -ne 2) { exit 20 }
$workerTargets = @(Select-LaunchedPython @($known[3], $comfy, $fakeChild) $known[2] 45 $source)
if ($workerTargets.Count -ne 1 -or $workerTargets[0].ProcessId -ne 3) { exit 21 }
exit 0
"#;
        let result = powershell(&format!("{definitions}\n{fixture}"))
            .unwrap()
            .status()
            .unwrap();
        assert!(
            result.success(),
            "attachment fixture exit code: {:?}",
            result.code()
        );
    }
}
