use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf, absolute};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;
use url::{Host, Url};

const DEFAULT_COMFY_URL: &str = "http://127.0.0.1:8188";
const SERVER: &str = "comfy_mcp_server.py";
const LAUNCHER: &str = "setup_render_bridge.py";
const REQUIREMENTS: &str = "requirements.txt";
const ACTIVE_RUNTIME: &str = "active-runtime";
const MCP_PORT_FILE: &str = "mcp-port";
const DEFAULT_MCP_PORT: u16 = 8189;
const RUNTIME_PREFIX: &str = "app-";
const INSTALL_CANCELLED: &str = "Setup cancelled. You can retry when ready.";
const REQUIREMENTS_TEXT: &str = include_str!("../../requirements.txt");
pub(crate) const RUNTIME_FILES: [(&str, &str); 5] = [
    (SERVER, include_str!("../../comfy_mcp_server.py")),
    (LAUNCHER, include_str!("../../setup_render_bridge.py")),
    (
        "verify_render_bridge.py",
        include_str!("../../verify_render_bridge.py"),
    ),
    (REQUIREMENTS, REQUIREMENTS_TEXT),
    ("LICENSE", include_str!("../../LICENSE")),
];
const PYTHON_PREREQUISITE_CHECK: &str = r#"
import json
import sys

version = list(sys.version_info[:3])
venv_ready = False
if tuple(version[:2]) >= (3, 11):
    try:
        import ensurepip
        import venv
        venv_ready = bool(ensurepip.version()) and callable(venv.EnvBuilder)
    except Exception:
        pass
print(json.dumps({"version": version, "executable": sys.executable, "venv_ready": venv_ready}))
"#;

#[derive(Debug, PartialEq, Eq)]
pub enum PythonReadiness {
    Ready {
        version: String,
        executable: PathBuf,
    },
    TooOld {
        version: String,
    },
    MissingVenv {
        version: String,
    },
    Missing,
    InvalidSelection,
}

#[derive(Deserialize)]
struct PythonProbe {
    version: [u32; 3],
    executable: PathBuf,
    venv_ready: bool,
}

impl PythonProbe {
    fn readiness(self) -> PythonReadiness {
        let [major, minor, patch] = self.version;
        let version = format!("{major}.{minor}.{patch}");
        if (major, minor) < (3, 11) {
            return PythonReadiness::TooOld { version };
        }
        if !self.venv_ready {
            return PythonReadiness::MissingVenv { version };
        }
        if python_executable(&self.executable) {
            PythonReadiness::Ready {
                version,
                executable: self.executable,
            }
        } else {
            PythonReadiness::InvalidSelection
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    #[serde(rename = "COMFY_ROOT")]
    pub comfy_root: String,
    #[serde(rename = "COMFY_URL")]
    pub comfy_url: String,
    #[serde(
        default,
        rename = "COMFY_INPUT_DIR",
        skip_serializing_if = "String::is_empty"
    )]
    pub comfy_input: String,
    #[serde(
        default,
        rename = "COMFY_OUTPUT_DIR",
        skip_serializing_if = "String::is_empty"
    )]
    pub comfy_output: String,
}

pub struct ModelCheck {
    pub filename: &'static str,
    pub available: Option<bool>,
}

#[derive(Clone)]
pub struct Installed {
    pub data_dir: PathBuf,
    pub app_dir: PathBuf,
    pub token_path: PathBuf,
    pub mcp_url: String,
    pub start_command: String,
}

fn environment_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Option<PathBuf> {
    environment_path(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
}

fn resolve_path(path: &Path) -> io::Result<PathBuf> {
    if let (Ok(suffix), Some(home)) = (path.strip_prefix("~"), home_dir()) {
        return absolute(home.join(suffix));
    }
    absolute(path)
}

pub(crate) fn data_dir() -> Result<PathBuf, String> {
    let path = environment_path("COMFY_MCP_DATA_DIR").map_or_else(default_data_dir, Ok)?;
    resolve_path(&path)
        .map_err(|_| "The Render Bridge user data directory could not be resolved.".into())
}

pub(crate) fn default_data_dir() -> Result<PathBuf, String> {
    let base = if cfg!(windows) {
        environment_path("LOCALAPPDATA")
            .or_else(|| home_dir().map(|home| home.join("AppData/Local")))
    } else {
        environment_path("XDG_DATA_HOME")
            .or_else(|| home_dir().map(|home| home.join(".local/share")))
    }
    .ok_or("A user data directory could not be found. Set COMFY_MCP_DATA_DIR and reopen setup.")?;
    Ok(base.join("comfy-mcp"))
}

fn read_settings(directory: &Path) -> Option<Settings> {
    read_setup_config(&directory.join("settings.json"))
        .ok()
        .flatten()
        .filter(|settings| {
            !settings.comfy_root.trim().is_empty() && !settings.comfy_url.trim().is_empty()
        })
}

fn read_setup_config(path: &Path) -> Result<Option<Settings>, String> {
    const LIMIT: u64 = 64 * 1024;
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Could not read the Render Bridge setup config.".into()),
    };
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err("Choose a regular Render Bridge settings JSON file up to 64 KiB.".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|_| "Could not read the Render Bridge setup config.")?;
    if bytes.len() as u64 > LIMIT {
        return Err("The Render Bridge setup config is too large.".into());
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    if bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        != Some(b'{')
    {
        return Err("The Render Bridge setup config must be a JSON object.".into());
    }
    serde_json::from_slice(bytes).map(Some).map_err(|_| {
        "Use a settings JSON object with only COMFY_ROOT, COMFY_URL, COMFY_INPUT_DIR, and COMFY_OUTPUT_DIR as strings.".into()
    })
}

pub(crate) fn fill_missing(mut settings: Settings, defaults: &Settings) -> Settings {
    if settings.comfy_root.trim().is_empty() {
        settings.comfy_root.clone_from(&defaults.comfy_root);
    }
    if settings.comfy_url.trim().is_empty() {
        settings.comfy_url.clone_from(&defaults.comfy_url);
    }
    for (saved, fallback) in [
        (&mut settings.comfy_input, &defaults.comfy_input),
        (&mut settings.comfy_output, &defaults.comfy_output),
    ] {
        if saved.trim().is_empty() {
            saved.clone_from(fallback);
        }
    }
    settings
}

pub(crate) fn import_setup_config(path: &Path, defaults: &Settings) -> Result<Settings, String> {
    let settings = read_setup_config(path)?.ok_or("The setup config could not be found.")?;
    for folder in [
        &settings.comfy_root,
        &settings.comfy_input,
        &settings.comfy_output,
    ] {
        let folder = folder.trim();
        let path = Path::new(folder);
        if !folder.is_empty() && !path.is_absolute() && path.strip_prefix("~").is_err() {
            return Err(
                "Use absolute folder paths or paths starting with ~/ in the setup config.".into(),
            );
        }
    }
    if !settings.comfy_url.trim().is_empty() {
        validate_comfy_url(&settings.comfy_url)?;
    }
    Ok(fill_missing(settings, defaults))
}

pub(crate) fn existing_setup_config(data: &Path) -> Result<Option<Settings>, String> {
    setup_snapshot(data).map(|(_, settings)| settings)
}

fn setup_snapshot(data: &Path) -> Result<(PathBuf, Option<Settings>), String> {
    let runtime = active_runtime(data)
        .map_err(|_| "Could not read the current Render Bridge runtime selection.")?;
    let saved = read_setup_config(&settings_directory(data, &runtime).join("settings.json"))?;
    if saved.is_none() && (runtime != data.join("app") || runtime.is_dir()) {
        return Err(
            "The current Render Bridge config is missing. Restore it before reinstalling.".into(),
        );
    }
    Ok((runtime, saved))
}

pub(crate) fn settings_for_setup(data: &Path, defaults: &Settings) -> Result<Settings, String> {
    Ok(existing_setup_config(data)?
        .map_or_else(|| defaults.clone(), |saved| fill_missing(saved, defaults)))
}

pub(crate) fn confirm_setup_settings(data: &Path, reviewed: &Settings) -> Result<Settings, String> {
    let effective = validate_settings(&settings_for_setup(data, reviewed)?)?;
    if effective != validate_settings(reviewed)? {
        return Err("The saved ComfyUI settings changed. Go back and check ComfyUI again before installing.".into());
    }
    Ok(effective)
}

fn active_runtime(directory: &Path) -> io::Result<PathBuf> {
    let marker = directory.join(ACTIVE_RUNTIME);
    let pointer = match fs::read_to_string(marker) {
        Ok(pointer) => pointer,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(directory.join("app")),
        Err(error) => return Err(error),
    };
    let name = pointer.trim();
    if !is_runtime_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid runtime name",
        ));
    }
    Ok(directory.join(name))
}

pub(crate) fn is_runtime_name(name: &str) -> bool {
    name.strip_prefix(RUNTIME_PREFIX).is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn settings_directory<'a>(data: &'a Path, runtime: &'a Path) -> &'a Path {
    if runtime == data.join("app") {
        data
    } else {
        runtime
    }
}

fn settings_for_runtime(data: &Path, runtime: &Path) -> Option<Settings> {
    read_settings(settings_directory(data, runtime))
}

pub fn default_settings() -> Settings {
    let comfy_root = environment_path("COMFY_ROOT")
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            home_dir()
                .unwrap_or_default()
                .join("ComfyUI")
                .to_string_lossy()
                .into_owned()
        });
    let comfy_url = env::var("COMFY_URL")
        .ok()
        .and_then(|value| validate_comfy_url(&value).ok())
        .unwrap_or_else(|| DEFAULT_COMFY_URL.into());
    Settings {
        comfy_root,
        comfy_url,
        comfy_input: String::new(),
        comfy_output: String::new(),
    }
}

pub(crate) fn validate_comfy_url(value: &str) -> Result<String, String> {
    const INVALID: &str =
        "Use a loopback HTTP(S) address without credentials, a path, query, or fragment.";
    let value = value.trim();
    let (_, authority) = value.split_once("://").ok_or(INVALID)?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    if value.bytes().any(|byte| !(33..=126).contains(&byte))
        || authority.contains(['@', '/', '\\', '?', '#'])
    {
        return Err(INVALID.into());
    }
    let url = Url::parse(value).map_err(|_| INVALID)?;
    let loopback = match url.host() {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if !loopback || !matches!(url.scheme(), "http" | "https") || url.port() == Some(0) {
        return Err(INVALID.into());
    }
    Ok(url.as_str().trim_end_matches('/').into())
}

pub fn validate_settings(settings: &Settings) -> Result<Settings, String> {
    let root = settings.comfy_root.trim();
    if root.is_empty() {
        return Err(
            "Choose the ComfyUI folder that contains its input and output directories.".into(),
        );
    }
    let root = resolve_path(Path::new(root))
        .map_err(|_| "The selected ComfyUI directory could not be resolved.")?;
    let comfy_root = root
        .to_str()
        .ok_or("The ComfyUI directory could not be represented as text.")?
        .into();
    let comfy_url = validate_comfy_url(&settings.comfy_url)?;
    let mut settings = Settings {
        comfy_root,
        comfy_url,
        ..settings.clone()
    };
    for kind in ["input", "output"] {
        let path = resolve_path(&folder(&settings, kind))
            .map_err(|_| format!("The ComfyUI {kind} folder could not be resolved."))?;
        if !path.is_dir() {
            return Err(format!(
                "Choose the ComfyUI {kind} folder that this instance uses."
            ));
        }
        let configured = if kind == "input" {
            &mut settings.comfy_input
        } else {
            &mut settings.comfy_output
        };
        if !configured.is_empty() {
            *configured = path
                .to_str()
                .ok_or("The ComfyUI folder is not a text path.")?
                .into();
        }
    }
    Ok(settings)
}

pub(crate) fn folder(settings: &Settings, kind: &str) -> PathBuf {
    let path = if kind == "input" {
        &settings.comfy_input
    } else {
        &settings.comfy_output
    };
    if path.trim().is_empty() {
        Path::new(&settings.comfy_root).join(kind)
    } else {
        PathBuf::from(path.trim())
    }
}

pub(crate) fn fetch_json(agent: &ureq::Agent, origin: &str, endpoint: &str) -> Option<Value> {
    let mut response = agent
        .get(format!("{origin}{endpoint}"))
        .header("Accept", "application/json")
        .call()
        .ok()?;
    if response.status() != 200 {
        return None;
    }
    response
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_json()
        .ok()
}

fn model_available(data: &Value, node: &str, field: &str, filename: &str) -> Option<bool> {
    let spec = &data[node]["input"]["required"][field];
    let choices = spec[0]
        .as_array()
        .or_else(|| spec[1]["options"].as_array())?;
    let mut found = false;
    for choice in choices {
        let basename = choice.as_str()?.rsplit(['/', '\\']).next();
        found |= basename == Some(filename);
    }
    Some(found)
}

pub(crate) fn loopback_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

pub struct Probe {
    pub models: Vec<ModelCheck>,
    pub reported: Option<crate::discovery::Instance>,
    pub hardware: Option<crate::models::Hardware>,
}

pub fn verify_folders(
    settings: &Settings,
    reported: Option<&crate::discovery::Instance>,
) -> Result<bool, String> {
    let settings = validate_settings(settings)?;
    if let Some(reported) = reported {
        for (kind, actual) in [("input", &reported.input), ("output", &reported.output)] {
            if fs::canonicalize(folder(&settings, kind)).ok().as_ref() != Some(actual) {
                return Err(format!(
                    "This API reports a different {kind} folder. Choose its detected instance or correct the paths."
                ));
            }
        }
        return Ok(true);
    }
    let agent = loopback_agent(Duration::from_secs(2));
    Ok(["input", "output"]
        .into_iter()
        .all(|kind| view_checks_folder(&agent, &settings, kind)))
}

fn view_checks_folder(agent: &ureq::Agent, settings: &Settings, kind: &str) -> bool {
    // Verify the backend resolves the chosen folder by reading a small marker
    // that this check owns. Dropping the tempfile removes it on every outcome.
    let lookup = || -> Option<bool> {
        let mut marker = tempfile::Builder::new()
            .prefix("render-bridge-setup-")
            .suffix(".txt")
            .rand_bytes(20)
            .tempfile_in(folder(settings, kind))
            .ok()?;
        let name = marker.path().file_name()?.to_str()?.to_owned();
        marker.write_all(name.as_bytes()).ok()?;
        // Close the file before ComfyUI opens it, including on Windows.
        let _cleanup = marker.into_temp_path();
        let mut response = agent
            .get(format!("{}/view", settings.comfy_url))
            .query("filename", &name)
            .query("type", kind)
            .call()
            .ok()?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(100)
            .read_to_vec()
            .ok()?;
        Some(bytes == name.as_bytes())
    };
    lookup().unwrap_or(false)
}

pub fn recheck_folders(settings: &Settings) -> Result<bool, String> {
    let agent = loopback_agent(Duration::from_secs(2));
    let Some(stats) = fetch_json(&agent, &settings.comfy_url, "/system_stats") else {
        return Ok(false);
    };
    let reported = crate::discovery::inspect(&settings.comfy_url, &stats);
    verify_folders(settings, reported.as_ref())
}

pub fn probe(settings: &Settings) -> Result<Probe, String> {
    let settings = validate_settings(settings)?;
    let agent = loopback_agent(Duration::from_secs(3));
    let origin = &settings.comfy_url;
    let stats = fetch_json(&agent, origin, "/system_stats").filter(|stats| stats["system"].is_object())
        .ok_or("ComfyUI did not report system information. Start ComfyUI and check the URL, or finish setup and start it later.")?;
    let reported = crate::discovery::inspect(origin, &stats);
    let models = crate::models::STARTER_PIPELINES
        .iter()
        .flat_map(|pipeline| pipeline.files)
        .map(|file| ModelCheck {
            filename: file.filename,
            available: fetch_json(&agent, origin, &format!("/object_info/{}", file.node))
                .and_then(|data| model_available(&data, file.node, file.field, file.filename)),
        })
        .collect();
    Ok(Probe {
        models,
        reported,
        hardware: crate::models::Hardware::from_stats(&stats),
    })
}

pub(crate) fn quiet_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
        command
            .env("PYTHON_MANAGER_AUTOMATIC_INSTALL", "false")
            .env_remove("PYLAUNCHER_ALLOW_INSTALL");
    }
    command
}

fn python_executable(path: &Path) -> bool {
    if !path.is_absolute() || !path.is_file() {
        return false;
    }
    // These OS launchers can open an installer or app store on a clean machine.
    #[cfg(windows)]
    if path.components().any(|part| {
        part.as_os_str()
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("WindowsApps"))
    }) {
        return false;
    }
    #[cfg(target_os = "macos")]
    if path == Path::new("/usr/bin/python3") {
        return false;
    }
    true
}

fn executable_on_path(name: &str, allowed: fn(&Path) -> bool) -> Vec<PathBuf> {
    let Some(path) = env::var_os("PATH") else {
        return Vec::new();
    };
    env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .filter(|candidate| allowed(candidate))
        .collect()
}

#[cfg(windows)]
pub fn winget_available() -> bool {
    environment_path("LOCALAPPDATA")
        .map(|directory| directory.join("Microsoft/WindowsApps/winget.exe"))
        .is_some_and(|path| path.is_file())
}

#[cfg(any(windows, target_os = "macos"))]
fn versioned_python(
    directory: &Path,
    suffix: &str,
    version: impl Fn(&str) -> Option<u32>,
) -> Vec<PathBuf> {
    let mut found: Vec<_> = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let minor = version(entry.file_name().to_str()?)?;
            let executable = entry.path().join(suffix);
            python_executable(&executable).then_some((std::cmp::Reverse(minor), executable))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, path)| path).collect()
}

fn python_candidates() -> Vec<(PathBuf, &'static [&'static str])> {
    let mut programs = Vec::new();
    #[cfg(windows)]
    {
        for name in ["python.exe", "python3.exe"] {
            programs.extend(executable_on_path(name, python_executable));
        }
        if let Some(local) = environment_path("LOCALAPPDATA") {
            programs.extend(versioned_python(
                &local.join("Programs/Python"),
                "python.exe",
                |name| {
                    name.strip_prefix("Python3")?
                        .split('-')
                        .next()?
                        .parse()
                        .ok()
                },
            ));
            programs.extend(versioned_python(
                &local.join("Python"),
                "python.exe",
                |name| {
                    name.strip_prefix("pythoncore-3.")?
                        .split(['-', '.'])
                        .next()?
                        .parse()
                        .ok()
                },
            ));
        }
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Some(program_files) = environment_path(variable) {
                programs.extend(versioned_python(&program_files, "python.exe", |name| {
                    name.strip_prefix("Python")?
                        .trim_start()
                        .strip_prefix('3')?
                        .trim_start_matches('.')
                        .split('-')
                        .next()?
                        .parse()
                        .ok()
                }));
            }
        }
    }
    #[cfg(not(windows))]
    for name in ["python3", "python"] {
        programs.extend(executable_on_path(name, python_executable));
    }
    #[cfg(target_os = "macos")]
    {
        programs.extend(
            [
                "/opt/homebrew/bin/python3",
                "/usr/local/bin/python3",
                "/Library/Developer/CommandLineTools/usr/bin/python3",
                "/Applications/Xcode.app/Contents/Developer/usr/bin/python3",
            ]
            .map(PathBuf::from),
        );
        programs.extend(versioned_python(
            Path::new("/Library/Frameworks/Python.framework/Versions"),
            "bin/python3",
            |name| name.strip_prefix("3.")?.parse().ok(),
        ));
    }
    let mut candidates = Vec::new();
    for program in programs {
        if !candidates.iter().any(|(known, _)| known == &program) && python_executable(&program) {
            candidates.push((program, &[] as &[&str]));
        }
    }
    #[cfg(windows)]
    {
        let mut launchers = executable_on_path("py.exe", python_executable);
        launchers.extend(environment_path("SystemRoot").map(|path| path.join("py.exe")));
        launchers.extend(
            environment_path("LOCALAPPDATA")
                .map(|path| path.join("Programs/Python/Launcher/py.exe")),
        );
        for launcher in launchers {
            if !candidates.iter().any(|(known, _)| known == &launcher)
                && python_executable(&launcher)
            {
                candidates.push((launcher, &["-3"]));
            }
        }
    }
    candidates
}

fn probe_python(
    program: &Path,
    arguments: &[&str],
    cancel: &AtomicBool,
) -> Option<PythonReadiness> {
    if !python_executable(program) {
        return None;
    }
    let mut output = tempfile::tempfile().ok()?;
    let mut command = quiet_command(program);
    command
        .args(arguments)
        .args(["-I", "-S", "-c", PYTHON_PREREQUISITE_CHECK])
        .stdout(Stdio::from(output.try_clone().ok()?));
    run_install_stage(
        &mut command,
        cancel,
        "Checking Python",
        Duration::from_secs(8),
    )
    .ok()?;
    output.rewind().ok()?;
    let probe: PythonProbe = serde_json::from_reader(output.take(16 * 1024)).ok()?;
    Some(probe.readiness())
}

fn resolve_python(selected: Option<&Path>, cancel: &AtomicBool) -> PythonReadiness {
    let configured = environment_path("RENDER_BRIDGE_PYTHON");
    if let Some(program) = selected.or(configured.as_deref()) {
        if let Ok(path) = resolve_path(program)
            && let Some(readiness) = probe_python(&path, &[], cancel)
        {
            return readiness;
        }
        // The environment override previously accepted a command name.
        if selected.is_none()
            && program.components().count() == 1
            && let Some(mut name) = program.to_str().map(str::to_owned)
        {
            if cfg!(windows) && program.extension().is_none() {
                name.push_str(".exe");
            }
            for path in executable_on_path(&name, python_executable) {
                if let Some(readiness) = probe_python(&path, &[], cancel) {
                    return readiness;
                }
            }
        }
        return PythonReadiness::InvalidSelection;
    }
    let mut best = PythonReadiness::Missing;
    for (program, arguments) in python_candidates() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match probe_python(&program, arguments, cancel) {
            Some(ready @ PythonReadiness::Ready { .. }) => return ready,
            Some(status @ PythonReadiness::MissingVenv { .. }) => best = status,
            Some(status @ PythonReadiness::TooOld { .. }) if best == PythonReadiness::Missing => {
                best = status
            }
            _ => {}
        }
    }
    best
}

pub fn check_python(selected: Option<&Path>) -> PythonReadiness {
    resolve_python(selected, &AtomicBool::new(false))
}

fn python_for_creation(selected: Option<&Path>, cancel: &AtomicBool) -> Result<Command, String> {
    let readiness = resolve_python(selected, cancel);
    check_cancel(cancel)?;
    let message = match readiness {
        PythonReadiness::Ready { executable, .. } => return Ok(quiet_command(executable)),
        PythonReadiness::Missing => "Python 3.11 or newer was not found.",
        PythonReadiness::TooOld { .. } => {
            "This Python version is too old. Use Python 3.11 or newer."
        }
        PythonReadiness::MissingVenv { .. } => {
            "Python needs venv and ensurepip. On Debian or Ubuntu, install the matching python3-venv package."
        }
        PythonReadiness::InvalidSelection => {
            "The selected Python executable could not be used. Choose an installed Python executable."
        }
    };
    Err(format!(
        "{message} Return to Prerequisites to install or select Python."
    ))
}

pub(crate) fn venv_python(app_dir: &Path) -> PathBuf {
    app_dir.join(if cfg!(windows) {
        ".venv/Scripts/python.exe"
    } else {
        ".venv/bin/python"
    })
}

pub(crate) fn atomic_write(destination: &Path, content: &[u8]) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("missing destination directory"))?;
    let mut file = NamedTempFile::new_in(parent)?;
    file.write_all(content)?;
    file.as_file().sync_all()?;
    file.persist(destination).map_err(|error| error.error)?;
    Ok(())
}

fn save_settings(directory: &Path, settings: &Settings) -> io::Result<()> {
    let mut content = serde_json::to_vec_pretty(settings)?;
    content.push(b'\n');
    atomic_write(&directory.join("settings.json"), &content)
}

pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn console_quote(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .ok_or("The application directory could not be represented as text.")?;
    let quote = if cfg!(windows) { "''" } else { "'\"'\"'" };
    let mut value = value.to_owned();
    if cfg!(windows) {
        for quote in ['\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'] {
            value = value.replace(quote, &format!("{quote}{quote}"));
        }
    }
    Ok(format!("'{}'", value.replace('\'', quote)))
}

fn start_command(
    data_dir: &Path,
    app_dir: &Path,
    python: &Path,
    token_path: &Path,
    port: u16,
) -> Result<String, String> {
    let data = console_quote(data_dir)?;
    let program = console_quote(python)?;
    let script = console_quote(&app_dir.join(LAUNCHER))?;
    let token = console_quote(token_path)?;
    if cfg!(windows) {
        Ok(format!(
            "$env:COMFY_MCP_DATA_DIR = {data}; $env:MCP_HOST = '127.0.0.1'; \
             $env:MCP_PORT = '{port}'; $env:MCP_TOKEN_FILE = {token}; \
             & {program} {script} start"
        ))
    } else {
        Ok(format!(
            "COMFY_MCP_DATA_DIR={data} MCP_HOST='127.0.0.1' MCP_PORT='{port}' \
             MCP_TOKEN_FILE={token} {program} {script} start"
        ))
    }
}

fn mcp_port(data_dir: &Path) -> Result<u16, String> {
    let raw_port = env::var("MCP_PORT").ok();
    mcp_port_with_env(data_dir, raw_port.as_deref())
}

fn mcp_port_with_env(data_dir: &Path, inherited: Option<&str>) -> Result<u16, String> {
    fn parse_port(value: &str) -> Option<u16> {
        let digits = value.trim();
        if digits.len() > 5 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u16>().ok().filter(|port| *port != 0)
    }

    let mut saved = String::new();
    match fs::File::open(data_dir.join(MCP_PORT_FILE)) {
        Ok(file) => {
            const READ_LIMIT: usize = 16;
            let invalid = || "The saved Render Bridge MCP port is invalid.".to_owned();
            file.take(READ_LIMIT as u64)
                .read_to_string(&mut saved)
                .map_err(|_| invalid())?;
            if saved.len() == READ_LIMIT {
                return Err(invalid());
            }
            parse_port(&saved).ok_or_else(invalid)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => inherited
            .map_or(Ok(DEFAULT_MCP_PORT), |port| {
                parse_port(port).ok_or_else(|| "MCP_PORT must be a valid port.".into())
            }),
        Err(_) => Err("The saved Render Bridge MCP port could not be read.".into()),
    }
}

fn save_mcp_port(data_dir: &Path, port: u16) -> Result<(), String> {
    if port != DEFAULT_MCP_PORT {
        atomic_write(
            &data_dir.join(MCP_PORT_FILE),
            format!("{port}\n").as_bytes(),
        )
        .map_err(|_| "Could not save the Render Bridge MCP port.".to_owned())?;
    }
    Ok(())
}

fn installation(data_dir: &Path, app_dir: PathBuf, port: u16) -> Result<Installed, String> {
    let python = venv_python(&app_dir);
    let token_path = if let Some(path) = environment_path("MCP_TOKEN_FILE") {
        let path = if path.is_absolute() || path.starts_with("~") {
            path
        } else {
            // A relative override used to resolve inside the legacy app.
            data_dir.join("app").join(path)
        };
        resolve_path(&path).map_err(|_| "The configured MCP token path could not be resolved.")?
    } else {
        data_dir.join("token.txt")
    };
    Ok(Installed {
        start_command: start_command(data_dir, &app_dir, &python, &token_path, port)?,
        mcp_url: format!("http://127.0.0.1:{port}/mcp"),
        token_path,
        data_dir: data_dir.to_path_buf(),
        app_dir,
    })
}

pub fn existing_install_at(data: &Path) -> Option<Installed> {
    if !data.is_absolute() {
        return None;
    }
    let app = active_runtime(data).ok()?;
    let installed = installation(data, app, mcp_port(data).ok()?).ok()?;
    settings_for_runtime(data, &installed.app_dir)?;
    (venv_python(&installed.app_dir).is_file()
        && RUNTIME_FILES
            .iter()
            .all(|(file, _)| installed.app_dir.join(file).is_file()))
    .then_some(installed)
}

#[cfg(windows)]
fn existing_legacy_install_for_repair(data: &Path) -> bool {
    let app = data.join("app");
    // The first release did not bundle the connection verifier. Recognize the
    // old layout for repair alone. It has no ownership marker for removal.
    crate::asset_folders::check_directory(data).is_ok()
        && crate::asset_folders::check_directory(&app).is_ok()
        && settings_for_runtime(data, &app).is_some()
        && venv_python(&app).is_file()
        && [SERVER, LAUNCHER, REQUIREMENTS, "LICENSE"]
            .iter()
            .all(|file| {
                app.join(file)
                    .symlink_metadata()
                    .is_ok_and(|metadata| metadata.is_file())
            })
}

pub fn installed_comfy_url(installed: &Installed) -> Option<String> {
    let settings = settings_for_runtime(&installed.data_dir, &installed.app_dir)?;
    validate_comfy_url(&settings.comfy_url).ok()
}

pub(crate) fn saved_install_settings(installed: &Installed) -> Result<Settings, String> {
    let settings = settings_for_runtime(&installed.data_dir, &installed.app_dir)
        .ok_or("Bridge setup settings are missing. Run setup again.")?;
    validate_settings(&settings)
}

pub(crate) fn config_file(installed: &Installed) -> PathBuf {
    settings_directory(&installed.data_dir, &installed.app_dir).join("settings.json")
}

#[cfg(windows)]
pub(crate) fn export_setup_config(installed: &Installed, path: &Path) -> Result<(), String> {
    let settings = saved_install_settings(installed)?;
    let mut bytes =
        serde_json::to_vec_pretty(&settings).map_err(|_| "Could not encode the setup config.")?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
        .map_err(|_| "Could not save the setup config in the selected folder.".into())
}

pub(crate) fn rotate_token(installed: &Installed) -> Result<(), String> {
    let port = url::Url::parse(&installed.mcp_url)
        .ok()
        .and_then(|address| address.port_or_known_default())
        .ok_or("The Render Bridge port is invalid.")?;
    run_install_stage(
        quiet_command(venv_python(&installed.app_dir))
            .current_dir(&installed.app_dir)
            .arg(installed.app_dir.join(LAUNCHER))
            .args(["rotate-token", "--confirm"])
            .env("COMFY_MCP_DATA_DIR", &installed.data_dir)
            .env("MCP_HOST", "127.0.0.1")
            .env("MCP_PORT", port.to_string())
            .env("MCP_TOKEN_FILE", &installed.token_path),
        &AtomicBool::new(false),
        "Token rotation",
        Duration::from_secs(30),
    )
    .map_err(|_| {
        "Could not rotate the token. Stop the server and check that its token file exists.".into()
    })
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err(INSTALL_CANCELLED.into())
    } else {
        Ok(())
    }
}

fn stop_install_stage(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // The unreaped child owns its process group. Its ID cannot be reused.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }

    // Windows Python launchers can spawn a separate interpreter. Stop the
    // tree rooted at this stage's still-owned process before reaping it.
    #[cfg(windows)]
    if matches!(child.try_wait(), Ok(None))
        && let Some(system) = environment_path("SystemRoot")
    {
        let _ = quiet_command(system.join("System32/taskkill.exe"))
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn run_install_stage(
    command: &mut Command,
    cancel: &AtomicBool,
    phase: &str,
    timeout: Duration,
) -> Result<(), String> {
    check_cancel(cancel)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|_| {
        format!("{phase} could not start. Check the Python executable and setup permissions.")
    })?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                check_cancel(cancel)?;
                return if status.success() {
                    Ok(())
                } else {
                    let code = status
                        .code()
                        .map_or_else(|| "interrupted".into(), |code| format!("exit code {code}"));
                    Err(format!(
                        "{phase} failed ({code}). Check Python, write permissions, and access to your package index before retrying."
                    ))
                };
            }
            Ok(None) => {}
            Err(_) => {
                stop_install_stage(&mut child);
                return Err(format!("{phase} stopped unexpectedly. Retry setup."));
            }
        }
        if cancel.load(Ordering::Relaxed) {
            stop_install_stage(&mut child);
            return Err(INSTALL_CANCELLED.into());
        }
        if Instant::now() >= deadline {
            stop_install_stage(&mut child);
            return Err(format!(
                "{phase} timed out. Check your connection and retry setup."
            ));
        }
        thread::sleep(Duration::from_millis(75));
    }
}

#[cfg(any(not(windows), test))]
fn activate_runtime(
    directory: &Path,
    staging: tempfile::TempDir,
    settings: &Settings,
) -> io::Result<()> {
    publish_runtime(directory, staging.path(), settings)?;
    // Existing shortcut and command paths can still point to a previous generation.
    let _ = staging.keep();
    Ok(())
}

fn publish_runtime(directory: &Path, runtime: &Path, settings: &Settings) -> io::Result<()> {
    save_settings(runtime, settings)?;
    let name = runtime
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid runtime name"))?;
    atomic_write(&directory.join(ACTIVE_RUNTIME), name.as_bytes())
}

pub(crate) fn lock_install(data_dir: &Path) -> Result<fs::File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    let lock = options
        .open(data_dir.join("install.lock"))
        .map_err(|_| "Could not open the Render Bridge install lock.")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if !lock.metadata().is_ok_and(|metadata| {
            metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
        }) {
            return Err("The existing install lock is not an owned regular file.".into());
        }
    }
    lock.try_lock().map_err(
        |_| "Could not lock the runtime for setup. Close any other setup wizard and retry.",
    )?;
    Ok(lock)
}

fn stage_runtime(
    data_dir: &Path,
    port: u16,
    mut command: Command,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&str),
) -> Result<(tempfile::TempDir, Installed), String> {
    let staging = tempfile::Builder::new()
        .prefix(RUNTIME_PREFIX)
        .tempdir_in(data_dir)
        .map_err(|_| "Could not create a staging directory for this upgrade.")?;
    let installed = installation(data_dir, staging.path().to_path_buf(), port)?;
    let python = venv_python(&installed.app_dir);
    for (name, content) in RUNTIME_FILES {
        check_cancel(cancel)?;
        atomic_write(&installed.app_dir.join(name), content.as_bytes())
            .map_err(|_| format!("Could not stage {name}. Check the app directory permissions."))?;
    }
    progress("Preparing a fresh Python environment.");
    run_install_stage(
        command
            .args(["-m", "venv"])
            .arg(installed.app_dir.join(".venv")),
        cancel,
        "Python environment creation",
        Duration::from_secs(120),
    )?;

    progress("Installing Python dependencies.");
    run_install_stage(
        quiet_command(&python)
            .args([
                "-m",
                "pip",
                "--disable-pip-version-check",
                "--no-input",
                "install",
                "--timeout",
                "20",
                "--retries",
                "1",
                "-r",
            ])
            .arg(installed.app_dir.join(REQUIREMENTS)),
        cancel,
        "Dependency installation",
        Duration::from_secs(600),
    )?;

    progress("Checking the staged Render Bridge runtime.");
    run_install_stage(
        quiet_command(&python).args(["-m", "pip", "--disable-pip-version-check", "check"]),
        cancel,
        "Dependency verification",
        Duration::from_secs(45),
    )?;
    let smoke_data = tempfile::Builder::new()
        .prefix(".smoke-")
        .tempdir_in(&installed.app_dir)
        .map_err(|_| "Could not prepare the runtime check.")?;
    run_install_stage(
        quiet_command(&python)
            .current_dir(&installed.app_dir)
            .env("COMFY_MCP_DATA_DIR", smoke_data.path())
            .env("MCP_TOKEN_FILE", smoke_data.path().join("token.txt"))
            .env("MCP_PORT", port.to_string())
            .arg(installed.app_dir.join(SERVER))
            .arg("--help"),
        cancel,
        "Render Bridge startup check",
        Duration::from_secs(30),
    )?;
    drop(smoke_data);
    #[cfg(windows)]
    crate::uninstall::mark_runtime(&installed.app_dir)?;

    Ok((staging, installed))
}

pub fn install(
    settings: &Settings,
    selected_python: Option<&Path>,
    selected_data_dir: Option<&Path>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&str),
) -> Result<Installed, String> {
    check_cancel(cancel)?;
    progress("Checking Python 3.11 or newer.");
    let command = python_for_creation(selected_python, cancel)?;
    let data_dir = match selected_data_dir {
        Some(path) => {
            if !path.is_absolute() {
                return Err("Choose an absolute path for the Render Bridge installation.".into());
            }
            match crate::asset_folders::check_directory(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(
                        "Choose a regular directory for the Render Bridge installation.".into(),
                    );
                }
                Ok(()) => {
                    let entries = fs::read_dir(path).map_err(
                        |_| "Choose a writable directory for the Render Bridge installation.",
                    )?;
                    #[cfg(windows)]
                    let retained_installation = crate::uninstall::is_owned(path)
                        || existing_legacy_install_for_repair(path);
                    #[cfg(not(windows))]
                    let retained_installation = false;
                    if entries.into_iter().any(|entry| {
                        entry.map_or(true, |entry| {
                            !matches!(
                                entry.file_name().to_str(),
                                Some("install.lock" | "settings.json")
                            )
                        })
                    }) && existing_install_at(path).is_none()
                        && !retained_installation
                    {
                        return Err(
                            "Choose an empty folder or an existing Render Bridge installation."
                                .into(),
                        );
                    }
                }
            }
            path.to_path_buf()
        }
        None => data_dir()?,
    };
    create_private_dir(&data_dir)
        .map_err(|_| "Could not create the Render Bridge data directory.")?;
    let _lock = lock_install(&data_dir)?;
    let snapshot = setup_snapshot(&data_dir)?;
    let settings = confirm_setup_settings(&data_dir, settings)?;
    let port = mcp_port(&data_dir)?;
    #[cfg(windows)]
    crate::uninstall::mark_install(&data_dir)?;
    let (staging, installed) = stage_runtime(&data_dir, port, command, cancel, &mut progress)?;
    check_cancel(cancel)?;
    progress("Activating the verified Render Bridge runtime.");
    if setup_snapshot(&data_dir)? != snapshot {
        return Err(
            "The saved setup changed while preparing the runtime. Check ComfyUI again and retry."
                .into(),
        );
    }
    save_mcp_port(&data_dir, port)?;
    #[cfg(windows)]
    activate_windows_runtime(&data_dir, staging, &settings, || {
        crate::update_handoff::replace_monitor(&installed)
    })?;
    #[cfg(not(windows))]
    activate_runtime(&data_dir, staging, &settings)
        .map_err(|_| "Could not activate the verified runtime. Retry setup.")?;
    Ok(installed)
}

#[cfg(windows)]
fn activate_windows_runtime(
    data_dir: &Path,
    staging: tempfile::TempDir,
    settings: &Settings,
    update_monitor: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let previous_marker = match fs::read(data_dir.join(ACTIVE_RUNTIME)) {
        Ok(marker) => Some(marker),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => {
            return Err("Could not read the previous Render Bridge runtime selection.".into());
        }
    };
    publish_runtime(data_dir, staging.path(), settings)
        .map_err(|_| "Could not activate the verified runtime. Retry the update.")?;
    if let Err(error) = update_monitor() {
        let marker = data_dir.join(ACTIVE_RUNTIME);
        let rollback = match previous_marker.as_deref() {
            Some(previous) => atomic_write(&marker, previous),
            None => fs::remove_file(marker),
        };
        if rollback.is_ok() {
            return Err(error);
        }
        // The new pointer may still be active. Keep its verified runtime usable.
        let _ = staging.keep();
        return Err(format!(
            "{error} The update could not restore the previous runtime selection. The verified runtime is still available."
        ));
    }
    let _ = staging.keep();
    Ok(())
}

/// Upgrade the selected installation with the new setup binary's bundled runtime.
///
/// The monitor callback runs under the install lock after the runtime is activated.
/// The callback must atomically replace the owned monitor and preserve it on error.
#[cfg(windows)]
pub fn upgrade_existing(
    data_dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&str),
    update_monitor: impl FnOnce(&Installed) -> Result<(), String>,
) -> Result<Installed, String> {
    check_cancel(cancel)?;
    if !data_dir.is_absolute() || !data_dir.is_dir() {
        return Err("The existing Render Bridge data directory could not be located.".into());
    }
    let _lock = lock_install(data_dir)?;
    let port = mcp_port(data_dir)?;
    let snapshot = setup_snapshot(data_dir)?;
    // The previous version may predate some files bundled in this updater.
    let settings = snapshot
        .1
        .as_ref()
        .ok_or("Bridge setup settings are missing. Run setup again.")?;
    let settings = validate_settings(settings)?;
    crate::uninstall::mark_install(data_dir)?;
    progress("Checking the existing Python installation.");
    let interpreter = venv_python(&snapshot.0);
    let command = python_for_creation(Some(&interpreter), cancel)
        .or_else(|_| python_for_creation(None, cancel))?;
    let (staging, installed) = stage_runtime(data_dir, port, command, cancel, &mut progress)?;
    check_cancel(cancel)?;
    progress("Activating the verified Render Bridge runtime.");
    if setup_snapshot(data_dir)? != snapshot {
        return Err("The saved setup changed while preparing the update. Review the configuration and retry.".into());
    }
    save_mcp_port(data_dir, port)?;
    activate_windows_runtime(data_dir, staging, &settings, || update_monitor(&installed))?;
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn setup_config_only_fills_missing_fields_and_preserves_an_invalid_original() {
        let data = tempfile::tempdir().unwrap();
        let default = Settings {
            comfy_root: data.path().join("ComfyUI").to_string_lossy().into_owned(),
            comfy_url: DEFAULT_COMFY_URL.into(),
            comfy_input: String::new(),
            comfy_output: String::new(),
        };
        let path = data.path().join("settings.json");
        fs::write(
            &path,
            r#"{"COMFY_URL":"http://127.0.0.1:8288","COMFY_INPUT_DIR":" "}"#,
        )
        .unwrap();
        let merged = settings_for_setup(data.path(), &default).unwrap();
        assert_eq!(merged.comfy_url, "http://127.0.0.1:8288");
        assert_eq!(merged.comfy_root, default.comfy_root);
        assert!(merged.comfy_input.is_empty());
        assert!(fs::read_to_string(&path).unwrap().contains("8288"));
        let supplied_input = data.path().join("supplied-input").display().to_string();
        let saved_root = data.path().join("existing-comfy").display().to_string();
        save_settings(
            data.path(),
            &Settings {
                comfy_root: saved_root.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        let merged = settings_for_setup(
            data.path(),
            &Settings {
                comfy_input: supplied_input.clone(),
                ..default.clone()
            },
        )
        .unwrap();
        assert_eq!(merged.comfy_root, saved_root);
        assert_eq!(merged.comfy_input, supplied_input);

        fs::write(
            &path,
            r#"{"COMFY_URL":"http://127.0.0.1:8288","MCP_TOKEN":"hidden"}"#,
        )
        .unwrap();
        assert!(settings_for_setup(data.path(), &default).is_err());
        assert!(fs::read_to_string(&path).unwrap().contains("MCP_TOKEN"));
    }

    #[test]
    fn setup_refuses_to_activate_settings_different_from_the_reviewed_backend() {
        let data = tempfile::tempdir().unwrap();
        fs::create_dir(data.path().join("input")).unwrap();
        fs::create_dir(data.path().join("output")).unwrap();
        let reviewed = Settings {
            comfy_root: data.path().display().to_string(),
            comfy_url: DEFAULT_COMFY_URL.into(),
            ..Default::default()
        };
        assert!(confirm_setup_settings(data.path(), &reviewed).is_ok());
        save_settings(
            data.path(),
            &Settings {
                comfy_url: "http://127.0.0.1:8288".into(),
                ..reviewed.clone()
            },
        )
        .unwrap();
        assert!(confirm_setup_settings(data.path(), &reviewed).is_err());
    }

    #[test]
    fn explicit_configs_reject_credentials_and_shell_relative_paths() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("settings.json");
        let defaults = default_settings();
        fs::write(&path, r#"{"COMFY_ROOT":"relative/ComfyUI"}"#).unwrap();
        assert!(import_setup_config(&path, &defaults).is_err());
        fs::write(
            &path,
            r#"{"COMFY_URL":"http://user:secret@127.0.0.1:8188"}"#,
        )
        .unwrap();
        assert!(import_setup_config(&path, &defaults).is_err());
        fs::write(&path, r#"{"COMFY_ROOT":"~/ComfyUI"}"#).unwrap();
        assert!(import_setup_config(&path, &defaults).is_ok());
    }

    #[test]
    fn python_readiness_distinguishes_versions_and_environment_support() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("python");
        fs::write(&executable, "").unwrap();
        let probe = |version, venv_ready| PythonProbe {
            version,
            executable: executable.clone(),
            venv_ready,
        };
        assert_eq!(
            probe([3, 10, 9], true).readiness(),
            PythonReadiness::TooOld {
                version: "3.10.9".into()
            },
        );
        assert_eq!(
            probe([3, 13, 2], false).readiness(),
            PythonReadiness::MissingVenv {
                version: "3.13.2".into()
            },
        );
        assert_eq!(
            probe([3, 11, 0], true).readiness(),
            PythonReadiness::Ready {
                version: "3.11.0".into(),
                executable: executable.clone()
            },
        );
        fs::remove_file(&executable).unwrap();
        assert_eq!(
            probe([3, 11, 0], true).readiness(),
            PythonReadiness::InvalidSelection
        );
    }

    #[test]
    fn unavailable_explicit_python_does_not_fall_back_to_a_different_installation() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            check_python(Some(&directory.path().join("missing-python"))),
            PythonReadiness::InvalidSelection,
        );
    }

    #[cfg(windows)]
    #[test]
    fn never_runs_an_execution_alias_from_windowsapps() {
        let directory = tempfile::tempdir().unwrap();
        let aliases = directory.path().join("Microsoft/wInDoWsApPs");
        fs::create_dir_all(&aliases).unwrap();
        let alias = aliases.join("python.exe");
        fs::write(&alias, "fixture execution alias").unwrap();
        assert_eq!(
            check_python(Some(&alias)),
            PythonReadiness::InvalidSelection
        );
    }

    #[cfg(unix)]
    #[test]
    fn both_check_and_install_resolve_the_chosen_absolute_interpreter() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("python");
        let output = serde_json::json!({
            "version": [3, 13, 4],
            "executable": executable,
            "venv_ready": true,
        })
        .to_string()
        .replace('\'', "'\"'\"'");
        fs::write(&executable, format!(
            "#!/bin/sh\n[ \"$1\" = -I ] && [ \"$2\" = -S ] && [ \"$3\" = -c ] || exit 1\nprintf '%s\\n' '{output}'\n"
        )).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            check_python(Some(&executable)),
            PythonReadiness::Ready {
                version: "3.13.4".into(),
                executable: executable.clone()
            },
        );
        let command = python_for_creation(Some(&executable), &AtomicBool::new(false)).unwrap();
        assert_eq!(command.get_program(), executable.as_os_str());
    }

    #[test]
    fn upgrade_activation_keeps_previous_runtime_settings_and_token() {
        let data = tempfile::tempdir().unwrap();
        let previous = tempfile::Builder::new()
            .prefix(RUNTIME_PREFIX)
            .tempdir_in(data.path())
            .unwrap();
        let previous_path = previous.path().to_path_buf();
        let token = data.path().join("token.txt");
        fs::write(&token, b"fixture-token").unwrap();
        let settings = Settings {
            comfy_root: "fixture".into(),
            comfy_url: "http://localhost:8188".into(),
            comfy_input: String::new(),
            comfy_output: String::new(),
        };
        activate_runtime(data.path(), previous, &settings).unwrap();

        let broken = tempfile::Builder::new()
            .prefix(RUNTIME_PREFIX)
            .tempdir_in(data.path())
            .unwrap();
        let broken_path = broken.path().to_path_buf();
        fs::create_dir(broken_path.join("settings.json")).unwrap();
        assert!(activate_runtime(data.path(), broken, &settings).is_err());
        assert!(!broken_path.exists());
        assert_eq!(active_runtime(data.path()).unwrap(), previous_path);
        assert!(
            settings_for_runtime(data.path(), &previous_path)
                .is_some_and(|stored| stored == settings)
        );

        let latest = tempfile::Builder::new()
            .prefix(RUNTIME_PREFIX)
            .tempdir_in(data.path())
            .unwrap();
        let latest_path = latest.path().to_path_buf();
        let changed = Settings {
            comfy_url: "http://localhost:9288".into(),
            ..settings.clone()
        };
        activate_runtime(data.path(), latest, &changed).unwrap();
        assert_eq!(active_runtime(data.path()).unwrap(), latest_path);
        assert!(
            settings_for_runtime(data.path(), &latest_path).is_some_and(|stored| stored == changed)
        );
        assert!(
            settings_for_runtime(data.path(), &previous_path)
                .is_some_and(|stored| stored == settings)
        );
        assert_eq!(fs::read(&token).unwrap(), b"fixture-token");
    }

    #[cfg(windows)]
    #[test]
    fn failed_monitor_update_restores_versioned_and_legacy_runtime_markers() {
        for legacy in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let token = data.path().join("token.txt");
            fs::write(&token, b"fixture-token").unwrap();
            let settings = Settings {
                comfy_root: "fixture".into(),
                comfy_url: "http://localhost:8188".into(),
                comfy_input: String::new(),
                comfy_output: String::new(),
            };
            let old_runtime = if legacy {
                let path = data.path().join("app");
                fs::create_dir(&path).unwrap();
                save_settings(data.path(), &settings).unwrap();
                path
            } else {
                let previous = tempfile::Builder::new()
                    .prefix(RUNTIME_PREFIX)
                    .tempdir_in(data.path())
                    .unwrap();
                let path = previous.path().to_path_buf();
                activate_runtime(data.path(), previous, &settings).unwrap();
                path
            };
            let staged = tempfile::Builder::new()
                .prefix(RUNTIME_PREFIX)
                .tempdir_in(data.path())
                .unwrap();
            let updated = staged.path().to_path_buf();
            let marker = data.path().join(ACTIVE_RUNTIME);
            let previous_marker = fs::read(&marker).ok();

            let error = activate_windows_runtime(data.path(), staged, &settings, || {
                assert_eq!(active_runtime(data.path()).unwrap(), updated);
                assert!(read_settings(&updated).is_some_and(|stored| stored == settings));
                Err("The monitor is still running.".into())
            });

            assert_eq!(error, Err("The monitor is still running.".into()));
            assert_eq!(active_runtime(data.path()).unwrap(), old_runtime);
            assert_eq!(fs::read(marker).ok(), previous_marker);
            assert_eq!(fs::read(token).unwrap(), b"fixture-token");
            assert!(old_runtime.is_dir());
            assert!(!updated.exists());
        }
    }

    #[test]
    fn start_command_carries_its_own_local_endpoint_and_token_location() {
        let data = Path::new("account's local data");
        let app = data.join("app");
        let token = data.join("a friend's token file");
        let command = start_command(data, &app, &venv_python(&app), &token, 19189).unwrap();
        assert!(!command.contains("PUBLIC_URLS"));
        if cfg!(windows) {
            assert!(command.contains("$env:MCP_HOST = '127.0.0.1'"));
            assert!(command.contains("$env:MCP_PORT = '19189'"));
            assert!(command.contains("$env:MCP_TOKEN_FILE = 'account''s local data"));
            assert!(command.contains("a friend''s token file'"));
        } else {
            assert!(command.contains("MCP_HOST='127.0.0.1'"));
            assert!(command.contains("MCP_PORT='19189'"));
            assert!(command.contains("MCP_TOKEN_FILE='account'\"'\"'s local data"));
            assert!(command.contains("a friend'\"'\"'s token file'"));
        }
        assert!(command.ends_with(" start"));
    }

    #[cfg(windows)]
    #[test]
    fn recognizes_the_first_release_layout_for_repair_only() {
        let data = tempfile::tempdir().unwrap();
        let app = data.path().join("app");
        let python = venv_python(&app);
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        fs::write(python, "").unwrap();
        for name in [SERVER, LAUNCHER, REQUIREMENTS, "LICENSE"] {
            fs::write(app.join(name), "").unwrap();
        }
        fs::write(
            data.path().join("settings.json"),
            r#"{"COMFY_ROOT":"sample","COMFY_URL":"http://127.0.0.1:8188"}"#,
        )
        .unwrap();
        assert!(existing_legacy_install_for_repair(data.path()));
        assert!(existing_install_at(data.path()).is_none());
        assert!(!crate::uninstall::is_owned(data.path()));
        fs::remove_file(app.join(REQUIREMENTS)).unwrap();
        assert!(!existing_legacy_install_for_repair(data.path()));
    }

    #[test]
    fn existing_install_uses_its_saved_mcp_port_after_runtime_repair() {
        let data = tempfile::tempdir().unwrap();
        let settings = Settings {
            comfy_root: data.path().display().to_string(),
            comfy_url: DEFAULT_COMFY_URL.into(),
            comfy_input: String::new(),
            comfy_output: String::new(),
        };
        assert_eq!(mcp_port_with_env(data.path(), None), Ok(8189));
        let port = mcp_port_with_env(data.path(), Some("19189")).unwrap();
        save_mcp_port(data.path(), port).unwrap();
        assert_eq!(mcp_port_with_env(data.path(), Some("8188")), Ok(port));

        for runtime in ["app-First", "app-Repaired"] {
            let directory = data.path().join(runtime);
            let python = venv_python(&directory);
            fs::create_dir_all(python.parent().unwrap()).unwrap();
            fs::write(python, b"fixture").unwrap();
            for (file, _) in RUNTIME_FILES {
                fs::write(directory.join(file), b"fixture").unwrap();
            }
            publish_runtime(data.path(), &directory, &settings).unwrap();
            let installed = existing_install_at(data.path()).unwrap();
            assert_eq!(installed.app_dir, directory);
            assert_eq!(installed.mcp_url, "http://127.0.0.1:19189/mcp");
            assert!(installed.start_command.contains("19189"));
        }
    }

    #[test]
    fn malformed_saved_port_does_not_fall_back_to_a_different_listener() {
        let data = tempfile::tempdir().unwrap();
        assert!(mcp_port_with_env(data.path(), Some("0")).is_err());
        for invalid in ["0", "65536", "8189/", "8189            trailing"] {
            fs::write(data.path().join(MCP_PORT_FILE), invalid).unwrap();
            assert!(mcp_port_with_env(data.path(), Some("8188")).is_err());
        }
    }

    #[test]
    fn url_allows_only_credential_free_loopback_origins() {
        assert_eq!(
            validate_comfy_url("http://localhost:8188/").unwrap(),
            "http://localhost:8188"
        );
        assert_eq!(
            validate_comfy_url("https://[::1]:8188").unwrap(),
            "https://[::1]:8188"
        );
        for invalid in [
            "http://user:secret@localhost:8188",
            "https://localhost:8188/private",
            "http://localhost?key=secret",
            "http://localhost#secret",
            "http://192.0.2.10:8188",
            "http://localhost.example",
            "http://localhost:0",
            "http://[::1]bad:8188",
        ] {
            assert!(validate_comfy_url(invalid).is_err());
        }
    }

    #[test]
    fn split_directories_are_valid_and_preserve_legacy_settings() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("custom-in");
        let output = directory.path().join("custom-out");
        fs::create_dir(&input).unwrap();
        fs::create_dir(&output).unwrap();
        let mut settings: Settings = serde_json::from_value(serde_json::json!({
            "COMFY_ROOT": directory.path(),
            "COMFY_URL": "http://localhost:8188",
        }))
        .unwrap();
        assert!(validate_settings(&settings).is_err());
        settings.comfy_input = input.to_string_lossy().into_owned();
        settings.comfy_output = output.to_string_lossy().into_owned();
        let validated = validate_settings(&settings).unwrap();
        assert_eq!(folder(&validated, "input"), input);
        assert_eq!(folder(&validated, "output"), output);
    }

    #[test]
    fn reported_directory_mismatch_blocks_setup() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["input", "output", "other-output"] {
            fs::create_dir(directory.path().join(name)).unwrap();
        }
        let settings = Settings {
            comfy_root: directory.path().to_string_lossy().into_owned(),
            comfy_url: DEFAULT_COMFY_URL.into(),
            comfy_input: String::new(),
            comfy_output: String::new(),
        };
        let mut instance = crate::discovery::Instance {
            url: settings.comfy_url.clone(),
            root: None,
            input: fs::canonicalize(folder(&settings, "input")).unwrap(),
            output: fs::canonicalize(folder(&settings, "output")).unwrap(),
        };
        assert_eq!(verify_folders(&settings, Some(&instance)), Ok(true));
        instance.output = fs::canonicalize(directory.path().join("other-output")).unwrap();
        assert!(
            verify_folders(&settings, Some(&instance))
                .unwrap_err()
                .contains("output")
        );
    }

    #[test]
    fn portable_folder_check_removes_its_marker() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input");
        let output = directory.path().join("output");
        fs::create_dir(&input).unwrap();
        fs::create_dir(&output).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let fixture_input = input.clone();
        let fixture_output = output.clone();
        let worker = thread::spawn(move || {
            for (kind, folder) in [("input", fixture_input), ("output", fixture_output)] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                assert!(request.contains("/view?filename=render-bridge-setup-"));
                assert!(request.contains(&format!("&type={kind} ")));
                let marker = fs::read_dir(folder)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                let contents = fs::read_to_string(marker).unwrap();
                loop {
                    request.clear();
                    reader.read_line(&mut request).unwrap();
                    if request == "\r\n" || request.is_empty() {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{contents}",
                    contents.len()
                )
                .unwrap();
            }
        });
        let settings = Settings {
            comfy_root: directory.path().to_string_lossy().into_owned(),
            comfy_url: format!("http://{address}"),
            comfy_input: String::new(),
            comfy_output: String::new(),
        };
        assert_eq!(verify_folders(&settings, None), Ok(true));
        worker.join().unwrap();
        assert_eq!(fs::read_dir(input).unwrap().count(), 0);
        assert_eq!(fs::read_dir(output).unwrap().count(), 0);
    }

    #[test]
    fn saving_backend_settings_preserves_existing_token_and_supports_retries() {
        let directory = tempfile::tempdir().unwrap();
        let token = directory.path().join("token.txt");
        fs::write(&token, b"fixture token; no real secret").unwrap();
        let settings = Settings {
            comfy_root: "fixture folder".into(),
            comfy_url: "http://localhost:8188".into(),
            comfy_input: String::new(),
            comfy_output: String::new(),
        };
        save_settings(directory.path(), &settings).unwrap();
        let updated = Settings {
            comfy_url: "http://localhost:9288".into(),
            ..settings
        };
        save_settings(directory.path(), &updated).unwrap();

        let persisted = read_settings(directory.path()).unwrap();
        assert_eq!(persisted.comfy_url, "http://localhost:9288");
        assert_eq!(persisted.comfy_root, "fixture folder");
        assert_eq!(fs::read(&token).unwrap(), b"fixture token; no real secret");
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(json.as_object().unwrap().len(), 2);
    }

    #[test]
    fn probe_distinguishes_found_missing_and_unknown_files_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        for folder in ["input", "output"] {
            fs::create_dir(directory.path().join(folder)).unwrap();
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for (path, body) in [
                ("/system_stats", r#"{"system":{}}"#),
                (
                    "/object_info/UNETLoader",
                    r#"{"UNETLoader":{"input":{"required":{"unet_name":[["a/SwarmUI_Z-Image-Turbo-FP8Mix.safetensors"]]}}}}"#,
                ),
                (
                    "/object_info/CLIPLoader",
                    r#"{"CLIPLoader":{"input":{"required":{"clip_name":["COMBO",{"options":[]}]}}}}"#,
                ),
                ("/object_info/VAELoader", r#"{}"#),
                (
                    "/object_info/CheckpointLoaderSimple",
                    r#"{"CheckpointLoaderSimple":{"input":{"required":{"ckpt_name":[["anime\\NetaYumev35_pretrained_all_in_one.safetensors"]]}}}}"#,
                ),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                assert_eq!(request, format!("GET {path} HTTP/1.1\r\n"));
                loop {
                    request.clear();
                    reader.read_line(&mut request).unwrap();
                    if request == "\r\n" || request.is_empty() {
                        break;
                    }
                    assert!(!request.to_ascii_lowercase().starts_with("authorization:"));
                }
                writeln!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                ).unwrap();
            }
        });
        let checks = probe(&Settings {
            comfy_root: directory.path().to_str().unwrap().into(),
            comfy_url: origin,
            comfy_input: String::new(),
            comfy_output: String::new(),
        })
        .unwrap();
        worker.join().unwrap();
        assert_eq!(
            checks
                .models
                .iter()
                .map(|check| check.available)
                .collect::<Vec<_>>(),
            vec![Some(true), Some(false), None, Some(true)],
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }
}
