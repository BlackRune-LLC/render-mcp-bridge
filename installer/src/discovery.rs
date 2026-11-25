use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::setup::{fetch_json, loopback_agent, validate_comfy_url};

const DEFAULT_ORIGINS: [&str; 2] = ["http://127.0.0.1:8188", "http://[::1]:8188"];
const MAX_DESKTOP_RECORDS: usize = 32;
const MAX_DESKTOP_PORTS: usize = 4;
const DESKTOP_RECORD_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub url: String,
    pub root: Option<PathBuf>,
    pub input: PathBuf,
    pub output: PathBuf,
}

pub fn discover(selected_url: &str) -> Vec<Instance> {
    let ports = desktop_records_dir()
        .map(|directory| desktop_ports(&directory))
        .unwrap_or_default();
    let agent = loopback_agent(Duration::from_millis(700));
    candidate_urls(selected_url, &ports)
        .into_iter()
        .filter_map(|url| {
            let stats = fetch_json(&agent, &url, "/system_stats")?;
            inspect(&url, &stats)
        })
        .collect()
}

pub fn inspect(url: &str, stats: &Value) -> Option<Instance> {
    let url = validate_comfy_url(url).ok()?;
    let argv = stats.get("system")?.get("argv")?.as_array()?;
    let argv = argv.iter().map(Value::as_str).collect::<Option<Vec<_>>>()?;
    let (program, arguments) = argv.split_first()?;
    let flags = path_flags(arguments)?;

    // ComfyUI forms the default paths relative to folder_paths.py. An
    // absolute main.py in the same checkout locates that directory.
    let base = match flags.base {
        Some(path) => canonical_directory(Path::new(path)),
        None => checkout_root(Path::new(program)),
    };
    let input = effective_directory(flags.input, base.as_deref(), "input")?;
    let output = effective_directory(flags.output, base.as_deref(), "output")?;
    let root = conventional_root(&input, &output);
    Some(Instance {
        url,
        root,
        input,
        output,
    })
}

#[derive(Default)]
struct PathFlags<'a> {
    base: Option<&'a str>,
    input: Option<&'a str>,
    output: Option<&'a str>,
}

fn path_flags<'a>(arguments: &[&'a str]) -> Option<PathFlags<'a>> {
    let mut flags = PathFlags::default();
    let mut pending = arguments.iter().copied();
    while let Some(arg) = pending.next() {
        if arg == "--" {
            break;
        }
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg, None), |(name, value)| (name, Some(value)));
        let slot = match name {
            "--base-directory" => &mut flags.base,
            "--input-directory" => &mut flags.input,
            "--output-directory" => &mut flags.output,
            _ => continue,
        };
        let value = inline.or_else(|| pending.next())?;
        if value.is_empty() || (inline.is_none() && value.starts_with('-')) {
            return None;
        }
        // These options use argparse's store action, so the last value wins.
        *slot = Some(value);
    }
    Some(flags)
}

fn effective_directory(
    override_path: Option<&str>,
    base: Option<&Path>,
    name: &str,
) -> Option<PathBuf> {
    match override_path {
        Some(path) => canonical_directory(Path::new(path)),
        None => canonical_directory(&base?.join(name)),
    }
}

fn checkout_root(program: &Path) -> Option<PathBuf> {
    if program.file_name()?.to_str()? != "main.py" || !absolute_local_path(program) {
        return None;
    }
    let program = fs::canonicalize(program).ok()?;
    let parent = program.parent()?;
    if !absolute_local_path(parent) || !parent.join("folder_paths.py").is_file() {
        return None;
    }
    Some(parent.to_path_buf())
}

fn canonical_directory(path: &Path) -> Option<PathBuf> {
    if !absolute_local_path(path) {
        return None;
    }
    let canonical = fs::canonicalize(path).ok()?;
    (absolute_local_path(&canonical) && canonical.is_dir()).then_some(canonical)
}

fn absolute_local_path(path: &Path) -> bool {
    if !path.is_absolute()
        || !path
            .to_str()
            .is_some_and(|text| !text.chars().any(char::is_control))
    {
        return false;
    }
    // Do not follow a network path named by a local HTTP response.
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        matches!(
            path.components().next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        )
    }
    #[cfg(not(windows))]
    {
        true
    }
}

fn conventional_root(input: &Path, output: &Path) -> Option<PathBuf> {
    let base = input.parent()?;
    (input == base.join("input") && output == base.join("output")).then(|| base.to_path_buf())
}

fn candidate_urls(selected_url: &str, desktop_ports: &[u16]) -> Vec<String> {
    let selected = validate_comfy_url(selected_url).ok();
    let mut urls = Vec::new();
    for url in selected
        .into_iter()
        .chain(DEFAULT_ORIGINS.map(str::to_owned))
        .chain(
            desktop_ports
                .iter()
                .copied()
                .filter(|port| *port != 0)
                .take(MAX_DESKTOP_PORTS)
                .map(|port| format!("http://127.0.0.1:{port}")),
        )
    {
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    urls
}

fn local_env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| absolute_local_path(path))
}

// Current open-source Comfy Desktop writes stateDir()/comfy-procs.
// Only the recorded port is a hint. The live ComfyUI response supplies paths.
fn desktop_records_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let state = local_env_path("APPDATA")
        .or_else(|| local_env_path("USERPROFILE").map(|home| home.join("AppData/Roaming")))?
        .join("Comfy Desktop");
    #[cfg(target_os = "macos")]
    let state = local_env_path("HOME")?.join("Library/Application Support/Comfy Desktop");
    #[cfg(target_os = "linux")]
    let state = local_env_path("XDG_STATE_HOME")
        .or_else(|| local_env_path("HOME").map(|home| home.join(".local/state")))?
        .join("comfyui-desktop-2");
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    return Some(state.join("comfy-procs"));
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    None
}

fn desktop_ports(directory: &Path) -> Vec<u16> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut hints = Vec::new();
    for entry in entries.take(MAX_DESKTOP_RECORDS).flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file())
            || entry.path().extension().is_none_or(|ext| ext != "json")
        {
            continue;
        }
        let Ok(file) = File::open(entry.path()) else {
            continue;
        };
        if let Ok(record) = serde_json::from_reader(file.take(DESKTOP_RECORD_BYTES)) {
            hints.extend(active_port(&record));
        }
    }
    hints.sort_unstable_by_key(|(time, _)| std::cmp::Reverse(*time));
    let mut ports = Vec::new();
    for (_, port) in hints {
        if !ports.contains(&port) {
            ports.push(port);
            if ports.len() == MAX_DESKTOP_PORTS {
                break;
            }
        }
    }
    ports
}

fn active_port(record: &Value) -> Option<(u64, u16)> {
    if record["v"].as_u64()? != 1
        || record["childPid"].as_u64()? <= 1
        || !record["stopRequestedAt"].is_null()
        || !record["childExitedAt"].is_null()
        || !absolute_local_path(Path::new(record["installPath"].as_str()?))
    {
        return None;
    }
    let port = u16::try_from(record["port"].as_u64()?).ok()?;
    (port != 0).then(|| (record["spawnedAt"].as_u64().unwrap_or_default(), port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn fixture_dir() -> TempDir {
        let fixture = tempfile::tempdir().unwrap();
        for name in ["input", "output"] {
            fs::create_dir(fixture.path().join(name)).unwrap();
        }
        fixture
    }

    #[test]
    fn base_and_split_overrides_follow_comfyui_precedence() {
        let fixture = fixture_dir();
        let base = fixture.path();
        let custom = base.join("custom");
        fs::create_dir(&custom).unwrap();
        let stats = json!({"system": {"argv": [
            "main.py", "--base-directory", base,
            "--input-directory", base.join("unused"),
            format!("--input-directory={}", custom.display()),
        ]}});
        let result = inspect("http://localhost:19512/", &stats).unwrap();
        assert_eq!(result.url, "http://localhost:19512");
        assert_eq!(result.input, custom.canonicalize().unwrap());
        assert_eq!(result.output, base.join("output").canonicalize().unwrap());
        assert_eq!(result.root, None);
    }

    #[test]
    fn absolute_launch_locates_the_checkout_even_in_standalone_mode() {
        let fixture = fixture_dir();
        for name in ["main.py", "folder_paths.py"] {
            fs::write(fixture.path().join(name), "").unwrap();
        }
        let stats = json!({"system": {"argv": [
            fixture.path().join("main.py"), "--windows-standalone-build",
        ]}});
        let result = inspect(DEFAULT_ORIGINS[0], &stats).unwrap();
        assert_eq!(result.root, Some(fixture.path().canonicalize().unwrap()));
    }

    #[test]
    fn unknown_and_relative_effective_paths_are_not_guessed() {
        let fixture = fixture_dir();
        for argv in [
            json!(["main.py"]),
            json!([
                "main.py",
                "--base-directory",
                fixture.path(),
                "--output-directory",
                "results"
            ]),
            json!([
                "main.py",
                "--base-directory",
                fixture.path(),
                "--input-directory",
                "--port",
                "8188"
            ]),
            json!([
                "main.py",
                "--base-directory",
                fixture.path(),
                "--output-directory="
            ]),
        ] {
            assert!(inspect(DEFAULT_ORIGINS[0], &json!({"system": {"argv": argv}})).is_none());
        }
        assert!(inspect(DEFAULT_ORIGINS[0], &json!({"system": {}})).is_none());
    }

    #[test]
    fn explicit_paths_resolve_without_a_known_checkout() {
        let fixture = fixture_dir();
        let stats = json!({"system": {"argv": [
            "main.py", "--base-directory", "relative",
            "--input-directory", fixture.path().join("input"),
            "--output-directory", fixture.path().join("output"),
        ]}});
        let result = inspect(DEFAULT_ORIGINS[0], &stats).unwrap();
        assert_eq!(result.root, Some(fixture.path().canonicalize().unwrap()));
    }

    #[test]
    fn endpoint_candidates_are_local_distinct_and_bounded_without_network() {
        assert_eq!(
            candidate_urls("https://example.com:8188", &[9191, 9192, 9193, 9194, 9195]),
            [
                "http://127.0.0.1:8188",
                "http://[::1]:8188",
                "http://127.0.0.1:9191",
                "http://127.0.0.1:9192",
                "http://127.0.0.1:9193",
                "http://127.0.0.1:9194",
            ],
        );
        assert_eq!(
            candidate_urls("http://127.0.0.1:9191/", &[9191]),
            [
                "http://127.0.0.1:9191",
                "http://127.0.0.1:8188",
                "http://[::1]:8188"
            ],
        );
        assert_eq!(
            candidate_urls("http://user@localhost:9191", &[]),
            DEFAULT_ORIGINS
        );
    }

    #[test]
    fn desktop_port_hints_ignore_stopped_and_invalid_metadata() {
        let directory = tempfile::tempdir().unwrap();
        for (index, port, stop) in [(1, 9201, None), (2, 9202, Some(5_u64)), (3, 9203, None)] {
            let record = json!({
                "v": 1, "childPid": 123, "installPath": directory.path(),
                "port": port, "spawnedAt": index, "stopRequestedAt": stop,
            });
            fs::write(
                directory.path().join(format!("{index}.json")),
                record.to_string(),
            )
            .unwrap();
        }
        fs::write(directory.path().join("invalid.json"), "partial").unwrap();
        assert_eq!(desktop_ports(directory.path()), vec![9203, 9201]);
    }
}
