use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{asset_folders, setup};

fn directory() -> Option<PathBuf> {
    setup::default_data_dir()
        .ok()
        .filter(|path| path.is_absolute())
        .map(|path| path.join("crash-logs"))
}

#[cfg(windows)]
pub(crate) fn existing_directory() -> Option<PathBuf> {
    directory().filter(|path| {
        path.parent()
            .is_some_and(|parent| asset_folders::check_directory(parent).is_ok())
            && asset_folders::check_directory(path).is_ok()
    })
}

#[cfg(windows)]
pub(crate) fn open_directory(folder: &Path) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};

    let folder = HSTRING::from(folder.as_os_str());
    // The folder string stays live for the shell call. No executable is searched.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(folder.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    result.0 as isize > 32
}

pub(crate) fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let origin = info
            .location()
            .map(|location| (location.file(), location.line()));
        record("panic", origin);
    }));
}

pub(crate) fn window_error() {
    record("window_error", None);
}

fn record(event: &'static str, origin: Option<(&str, u32)>) {
    if let Some(directory) = directory() {
        let _ = write_report(&directory, event, origin);
    }
}

fn write_report(
    directory: &Path,
    event: &'static str,
    origin: Option<(&str, u32)>,
) -> io::Result<()> {
    // Store source coordinates only. Panic payloads and window errors can
    // contain credentials, personal paths, or content passed into the app.
    let source = origin.and_then(|(path, line)| {
        let file = path.rsplit(['/', '\\']).next()?;
        (file.ends_with(".rs")
            && file.len() <= 64
            && file
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte)))
        .then(|| serde_json::json!({"file": file, "line": line}))
    });
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|time| time.as_secs());
    let report = serde_json::json!({
        "schema": 1,
        "component": "desktop",
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp_unix": timestamp,
        "event": event,
        "source": source,
    });
    let mut data = serde_json::to_vec_pretty(&report)?;
    data.push(b'\n');
    let parent = directory
        .parent()
        .ok_or_else(|| io::Error::other("missing crash data directory"))?;
    for folder in [parent, directory] {
        setup::create_private_dir(folder)?;
        asset_folders::check_directory(folder)?;
    }
    setup::atomic_write(&directory.join("desktop.json"), &data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_hook_omits_the_payload_in_a_failed_process() {
        const FIXTURE: &str = "RENDER_BRIDGE_PANIC_FIXTURE";
        const PRIVATE_PAYLOAD: &str = "sample-private-token-for-panic-test";
        if std::env::var_os(FIXTURE).is_some() {
            install_panic_hook();
            panic!("{PRIVATE_PAYLOAD}");
        }

        let root = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "diagnostics::tests::panic_hook_omits_the_payload_in_a_failed_process",
            ])
            .env(FIXTURE, "1")
            .env("LOCALAPPDATA", root.path())
            .env("XDG_DATA_HOME", root.path())
            .env("COMFY_MCP_DATA_DIR", root.path().join("custom"))
            .output()
            .unwrap();
        assert!(!result.status.success());
        let path = root.path().join("comfy-mcp/crash-logs/desktop.json");
        let contents = std::fs::read_to_string(path).unwrap();
        let report: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(report["event"], "panic");
        assert!(!contents.contains(PRIVATE_PAYLOAD));
        assert!(!String::from_utf8_lossy(&result.stdout).contains(PRIVATE_PAYLOAD));
        assert!(!String::from_utf8_lossy(&result.stderr).contains(PRIVATE_PAYLOAD));
        assert!(!root.path().join("custom").exists());
    }

    #[test]
    fn replaces_the_latest_report_and_strips_source_paths() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("crash-logs");
        write_report(
            &directory,
            "panic",
            Some((r"C:\private-folder\example.rs", 29)),
        )
        .unwrap();
        let path = directory.join("desktop.json");
        let contents = std::fs::read_to_string(&path).unwrap();
        let report: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(
            report["source"],
            serde_json::json!({"file": "example.rs", "line": 29})
        );
        assert!(!contents.contains("private-folder"));

        write_report(&directory, "window_error", None).unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(report["event"], "window_error");
        assert!(report["source"].is_null());
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 1);
    }
}
