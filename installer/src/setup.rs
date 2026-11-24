use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
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
const RUNTIME_PREFIX: &str = "app-";
const INSTALL_CANCELLED: &str = "Setup cancelled. You can retry when ready.";
const REQUIREMENTS_TEXT: &str = include_str!("../../requirements.txt");
const RUNTIME_FILES: [(&str, &str); 5] = [
    (SERVER, include_str!("../../comfy_mcp_server.py")),
    (LAUNCHER, include_str!("../../setup_render_bridge.py")),
    (
        "verify_render_bridge.py",
        include_str!("../../verify_render_bridge.py"),
    ),
    (REQUIREMENTS, REQUIREMENTS_TEXT),
    ("LICENSE", include_str!("../../LICENSE")),
];
const PYTHON_VERSION_CHECK: &str = "import sys; sys.exit(sys.version_info < (3, 11))";

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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

fn data_dir() -> Result<PathBuf, String> {
    let path = if let Some(override_path) = environment_path("COMFY_MCP_DATA_DIR") {
        override_path
    } else {
        let base = if cfg!(windows) {
            environment_path("LOCALAPPDATA")
                .or_else(|| home_dir().map(|home| home.join("AppData/Local")))
        } else {
            environment_path("XDG_DATA_HOME")
                .or_else(|| home_dir().map(|home| home.join(".local/share")))
        }
        .ok_or(
            "A user data directory could not be found. Set COMFY_MCP_DATA_DIR and reopen setup.",
        )?;
        base.join("comfy-mcp")
    };
    resolve_path(&path)
        .map_err(|_| "The Render Bridge user data directory could not be resolved.".into())
}

fn read_settings(directory: &Path) -> Option<Settings> {
    serde_json::from_slice(&fs::read(directory.join("settings.json")).ok()?).ok()
}

fn active_runtime(directory: &Path) -> io::Result<PathBuf> {
    let marker = directory.join(ACTIVE_RUNTIME);
    let pointer = match fs::read_to_string(marker) {
        Ok(pointer) => pointer,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(directory.join("app")),
        Err(error) => return Err(error),
    };
    let name = pointer.trim();
    let valid_name = name.strip_prefix(RUNTIME_PREFIX).is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    });
    if !valid_name {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid runtime name",
        ));
    }
    Ok(directory.join(name))
}

fn settings_for_runtime(data: &Path, runtime: &Path) -> Option<Settings> {
    read_settings(if runtime == data.join("app") {
        data
    } else {
        runtime
    })
}

pub fn default_settings() -> Settings {
    let saved = data_dir().ok().and_then(|data| {
        let runtime = active_runtime(&data).ok()?;
        settings_for_runtime(&data, &runtime)
    });
    let root_override = environment_path("COMFY_ROOT");
    let comfy_root = root_override
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| saved.as_ref().map(|settings| settings.comfy_root.clone()))
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
        .or_else(|| {
            saved
                .as_ref()
                .and_then(|settings| validate_comfy_url(&settings.comfy_url).ok())
        })
        .unwrap_or_else(|| DEFAULT_COMFY_URL.into());
    Settings {
        comfy_root,
        comfy_url,
        comfy_input: saved
            .as_ref()
            .filter(|_| root_override.is_none())
            .map(|s| s.comfy_input.clone())
            .unwrap_or_default(),
        comfy_output: saved
            .as_ref()
            .filter(|_| root_override.is_none())
            .map(|s| s.comfy_output.clone())
            .unwrap_or_default(),
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
    Ok(Probe { models, reported })
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

fn usable_python(program: impl AsRef<OsStr>, arguments: &[&str], cancel: &AtomicBool) -> bool {
    let mut command = quiet_command(program);
    command.args(arguments).args(["-c", PYTHON_VERSION_CHECK]);
    run_install_stage(
        &mut command,
        cancel,
        "Checking Python",
        Duration::from_secs(8),
    )
    .is_ok()
}

fn python_for_creation(cancel: &AtomicBool) -> Result<Command, String> {
    if let Some(program) = environment_path("RENDER_BRIDGE_PYTHON") {
        let usable = usable_python(&program, &[], cancel);
        check_cancel(cancel)?;
        return usable.then(|| quiet_command(&program)).ok_or(
            "RENDER_BRIDGE_PYTHON must name a working Python 3.11 or newer executable.".into(),
        );
    }
    let candidates: &[(&str, &[&str])] = if cfg!(windows) {
        &[("py", &["-3"]), ("python", &[]), ("python3", &[])]
    } else {
        &[("python3", &[]), ("python", &[])]
    };
    for &(program, arguments) in candidates {
        if usable_python(program, arguments, cancel) {
            let mut command = quiet_command(program);
            command.args(arguments);
            return Ok(command);
        }
        check_cancel(cancel)?;
    }
    Err("Python 3.11 or newer was not found. Install Python, reopen setup, or set RENDER_BRIDGE_PYTHON to its executable.".into())
}

pub(crate) fn venv_python(app_dir: &Path) -> PathBuf {
    app_dir.join(if cfg!(windows) {
        ".venv/Scripts/python.exe"
    } else {
        ".venv/bin/python"
    })
}

fn atomic_write(destination: &Path, content: &[u8]) -> io::Result<()> {
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

fn create_private_dir(path: &Path) -> io::Result<()> {
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
    let public_url = format!("local=http://127.0.0.1:{port}");
    if cfg!(windows) {
        Ok(format!(
            "$env:COMFY_MCP_DATA_DIR = {data}; $env:MCP_HOST = '127.0.0.1'; \
             $env:MCP_PORT = '{port}'; $env:MCP_TOKEN_FILE = {token}; \
             $env:PUBLIC_URLS = '{public_url}'; & {program} {script} start"
        ))
    } else {
        Ok(format!(
            "COMFY_MCP_DATA_DIR={data} MCP_HOST='127.0.0.1' MCP_PORT='{port}' \
             MCP_TOKEN_FILE={token} PUBLIC_URLS='{public_url}' {program} {script} start"
        ))
    }
}

fn mcp_port() -> Result<u16, String> {
    let raw_port = env::var("MCP_PORT").ok();
    raw_port
        .as_deref()
        .unwrap_or("8189")
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| "MCP_PORT must be a valid port.".into())
}

fn installation(data_dir: &Path, app_dir: PathBuf) -> Result<Installed, String> {
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
    let port = mcp_port()?;
    Ok(Installed {
        start_command: start_command(data_dir, &app_dir, &python, &token_path, port)?,
        mcp_url: format!("http://127.0.0.1:{port}/mcp"),
        token_path,
        data_dir: data_dir.to_path_buf(),
        app_dir,
    })
}

pub fn existing_install() -> Option<Installed> {
    let data = data_dir().ok()?;
    let app = active_runtime(&data).ok()?;
    let installed = installation(&data, app).ok()?;
    settings_for_runtime(&data, &installed.app_dir)?;
    (venv_python(&installed.app_dir).is_file()
        && RUNTIME_FILES
            .iter()
            .all(|(file, _)| installed.app_dir.join(file).is_file()))
    .then_some(installed)
}

pub(crate) fn saved_install_settings(installed: &Installed) -> Result<Settings, String> {
    let settings = settings_for_runtime(&installed.data_dir, &installed.app_dir)
        .ok_or("Bridge setup settings are missing. Run setup again.")?;
    validate_settings(&settings)
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

fn activate_runtime(
    directory: &Path,
    staging: tempfile::TempDir,
    settings: &Settings,
) -> io::Result<()> {
    save_settings(staging.path(), settings)?;
    let name = staging
        .path()
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid runtime name"))?;
    atomic_write(&directory.join(ACTIVE_RUNTIME), name.as_bytes())?;
    // Existing shortcut and command paths can still point to a previous generation.
    let _ = staging.keep();
    Ok(())
}

pub fn install(
    settings: &Settings,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&str),
) -> Result<Installed, String> {
    check_cancel(cancel)?;
    let settings = validate_settings(settings)?;
    let data_dir = data_dir()?;
    create_private_dir(&data_dir)
        .map_err(|_| "Could not create the Render Bridge data directory.")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join("install.lock"))
        .map_err(|_| "Could not open the Render Bridge install lock.")?;
    lock.try_lock().map_err(
        |_| "Could not lock the runtime for setup. Close any other setup wizard and retry.",
    )?;

    let staging = tempfile::Builder::new()
        .prefix(RUNTIME_PREFIX)
        .tempdir_in(&data_dir)
        .map_err(|_| "Could not create a staging directory for this upgrade.")?;
    let installed = installation(&data_dir, staging.path().to_path_buf())?;
    let python = venv_python(&installed.app_dir);
    for (name, content) in RUNTIME_FILES {
        check_cancel(cancel)?;
        atomic_write(&installed.app_dir.join(name), content.as_bytes())
            .map_err(|_| format!("Could not stage {name}. Check the app directory permissions."))?;
    }
    progress("Looking for Python 3.11 or newer.");
    let mut command = python_for_creation(cancel)?;
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
            .arg(installed.app_dir.join(SERVER))
            .arg("--help"),
        cancel,
        "Render Bridge startup check",
        Duration::from_secs(30),
    )?;
    drop(smoke_data);

    check_cancel(cancel)?;
    progress("Activating the verified Render Bridge runtime.");
    activate_runtime(&data_dir, staging, &settings)
        .map_err(|_| "Could not activate the verified runtime. Retry setup.")?;
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

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

    #[test]
    fn start_command_carries_its_own_local_endpoint_and_token_location() {
        let data = Path::new("account's local data");
        let app = data.join("app");
        let token = data.join("a friend's token file");
        let command = start_command(data, &app, &venv_python(&app), &token, 19189).unwrap();
        assert!(command.contains("local=http://127.0.0.1:19189"));
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
