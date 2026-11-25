use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;

use base64::{Engine, engine::general_purpose::STANDARD};
use windows::Win32::Foundation::{ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::core::{PCWSTR, w};

// Use only OS modules and the registered Microsoft App Installer package.
// Its executable and the repository are checked separately because the
// WinGet source name is configurable.
const INSTALL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$PSModuleAutoloadingPreference = 'None'
$env:Path = [Environment]::SystemDirectory
$modules = [IO.Path]::Combine($PSHOME, 'Modules')
try {
    Microsoft.PowerShell.Core\Import-Module ([IO.Path]::Combine($modules, 'Appx\Appx.psd1'))
    Microsoft.PowerShell.Core\Import-Module ([IO.Path]::Combine($modules, 'Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1'))
    $package = $null
    $packages = Appx\Get-AppxPackage -AllUsers -Name 'Microsoft.DesktopAppInstaller' -PackageTypeFilter Main
    foreach ($candidate in $packages) {
        if ($candidate.PackageFamilyName -eq 'Microsoft.DesktopAppInstaller_8wekyb3d8bbwe' -and
            ([string]$candidate.SignatureKind -in @('Store', 'System')) -and
            [IO.Path]::IsPathRooted($candidate.InstallLocation) -and
            ($null -eq $package -or [version]$candidate.Version -gt [version]$package.Version)) {
            $package = $candidate
        }
    }
    if ($null -eq $package) { exit 2 }
    $winget = [IO.Path]::Combine($package.InstallLocation, 'winget.exe')
    if (-not [IO.File]::Exists($winget)) { exit 2 }
} catch {
    exit 2
}
try {
    $export = & $winget source export winget --disable-interactivity
    if ($LASTEXITCODE -ne 0) { exit 3 }
    $source = ($export -join [Environment]::NewLine) | Microsoft.PowerShell.Utility\ConvertFrom-Json
    if ($source.Name -cne 'winget' -or
        $source.Arg -cne 'https://cdn.winget.microsoft.com/cache' -or
        $source.Identifier -cne 'Microsoft.Winget.Source_8wekyb3d8bbwe' -or
        $source.Type -cne 'Microsoft.PreIndexed.Package' -or
        $source.TrustLevel -cnotcontains 'Trusted' -or
        $source.TrustLevel -cnotcontains 'StoreOrigin') { exit 3 }
} catch {
    exit 3
}
try {
    & $winget install --id Python.Python.3.13 --exact --source winget --scope machine --interactive
    if ($LASTEXITCODE -ne 0) { exit 4 }
} catch {
    exit 4
}
exit 0
"#;

fn system_dir() -> Option<PathBuf> {
    let mut buffer = [0_u16; 260];
    // The buffer is writable for the number of UTF-16 units given to Windows.
    let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    let path = PathBuf::from(OsString::from_wide(&buffer[..length]));
    path.is_absolute().then_some(path)
}

struct ShellApartment;

impl ShellApartment {
    fn init() -> Result<Self, String> {
        // The prerequisite worker owns this thread and drops the apartment there.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(|_| "Windows could not prepare the administrator prompt.".to_owned())?;
        Ok(Self)
    }
}

impl Drop for ShellApartment {
    fn drop(&mut self) {
        // Matches the successful initialization on this thread.
        unsafe { CoUninitialize() };
    }
}

fn launch_arguments() -> Vec<u16> {
    let script: Vec<u8> = INSTALL_SCRIPT
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    format!(
        "-NoLogo -NoProfile -EncodedCommand {}",
        STANDARD.encode(script)
    )
    .encode_utf16()
    .chain(Some(0))
    .collect()
}

fn installer_status(code: u32) -> Result<(), String> {
    match code {
        0 => Ok(()),
        2 => Err(
            "Microsoft App Installer with WinGet is unavailable. Use the official Python download."
                .into(),
        ),
        3 => Err(
            "The official WinGet source could not be verified. Use the official Python download."
                .into(),
        ),
        _ => Err(
            "Python installation did not finish. Try again or use the official Python download."
                .into(),
        ),
    }
}

pub fn install_latest_python() -> Result<(), String> {
    let directory = system_dir().ok_or("Windows could not locate its system directory.")?;
    let shell = directory.join("WindowsPowerShell/v1.0/powershell.exe");
    if !shell.is_file() {
        return Err("Windows PowerShell is unavailable. Use the official Python download.".into());
    }
    let directory: Vec<_> = directory.as_os_str().encode_wide().chain(Some(0)).collect();
    let shell: Vec<_> = shell.as_os_str().encode_wide().chain(Some(0)).collect();
    let arguments = launch_arguments();
    let _apartment = ShellApartment::init()?;
    let mut request = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(shell.as_ptr()),
        lpParameters: PCWSTR(arguments.as_ptr()),
        lpDirectory: PCWSTR(directory.as_ptr()),
        nShow: 1,
        ..Default::default()
    };
    // All strings remain alive and NUL-terminated until ShellExecuteExW returns.
    unsafe { ShellExecuteExW(&mut request) }.map_err(|error| {
        if error.code() == ERROR_CANCELLED.to_hresult() {
            "Administrator approval was cancelled. You can retry or install Python manually."
                .to_owned()
        } else {
            "Windows could not start the administrator install. Use the official Python download."
                .to_owned()
        }
    })?;
    if request.hProcess.is_invalid() {
        return Err("Wait for the installer to close, then choose Check Python again.".into());
    }
    // ShellExecuteExW transferred one owned process handle via NOCLOSEPROCESS.
    let process = unsafe { OwnedHandle::from_raw_handle(request.hProcess.0) };
    let handle = HANDLE(process.as_raw_handle());
    // The worker waits on the owned handle while the main window stays responsive.
    if unsafe { WaitForSingleObject(handle, INFINITE) } != WAIT_OBJECT_0 {
        return Err(
            "Could not confirm setup finished. Close the installer, then choose Check Python again.".into(),
        );
    }
    let mut code = 0;
    // The handle remains valid and the output slot is writable.
    unsafe { GetExitCodeProcess(handle, &mut code) }
        .map_err(|_| "Could not confirm the Python setup result. Choose Check Python again.")?;
    installer_status(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn administrator_launch_has_only_the_fixed_script_and_disables_profiles() {
        let arguments = launch_arguments();
        assert_eq!(arguments.last(), Some(&0));
        let arguments = String::from_utf16(&arguments[..arguments.len() - 1]).unwrap();
        let encoded = arguments
            .strip_prefix("-NoLogo -NoProfile -EncodedCommand ")
            .unwrap();
        let script = STANDARD.decode(encoded).unwrap();
        let script: Vec<_> = script
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(String::from_utf16(&script).unwrap(), INSTALL_SCRIPT);
        assert!(INSTALL_SCRIPT.contains("source export winget --disable-interactivity"));
        assert!(
            INSTALL_SCRIPT.contains(
                "Python.Python.3.13 --exact --source winget --scope machine --interactive"
            )
        );
        assert!(!INSTALL_SCRIPT.contains("--accept-"));
    }

    #[test]
    fn failed_or_untrusted_package_manager_returns_manual_fallback() {
        assert!(installer_status(0).is_ok());
        assert!(installer_status(2).unwrap_err().contains("unavailable"));
        assert!(installer_status(3).unwrap_err().contains("verified"));
        assert!(installer_status(4).is_err());
    }
}
