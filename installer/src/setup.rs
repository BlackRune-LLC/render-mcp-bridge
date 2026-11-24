use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf, absolute};
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::NamedTempFile;
use url::{Host, Url};

const DEFAULT_COMFY_URL: &str = "http://127.0.0.1:8188";
const SERVER: &str = "comfy_mcp_server.py";
const LAUNCHER: &str = "setup_render_bridge.py";
const REQUIREMENTS: &str = "requirements.txt";
const REQUIREMENTS_TEXT: &str = include_str!("../../requirements.txt");
const RUNTIME_FILES: [(&str, &str); 4] = [
    (SERVER, include_str!("../../comfy_mcp_server.py")),
    (LAUNCHER, include_str!("../../setup_render_bridge.py")),
    (REQUIREMENTS, REQUIREMENTS_TEXT),
    ("LICENSE", include_str!("../../LICENSE")),
];
const STARTER_MODELS: [(&str, &str, &str); 4] = [
    (
        "UNETLoader",
        "unet_name",
        "SwarmUI_Z-Image-Turbo-FP8Mix.safetensors",
    ),
    ("CLIPLoader", "clip_name", "qwen_3_4b.safetensors"),
    ("VAELoader", "vae_name", "ae.safetensors"),
    (
        "CheckpointLoaderSimple",
        "ckpt_name",
        "NetaYumev35_pretrained_all_in_one.safetensors",
    ),
];
const PYTHON_VERSION_CHECK: &str = "import sys; sys.exit(sys.version_info < (3, 11))";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(rename = "COMFY_ROOT")]
    pub comfy_root: String,
    #[serde(rename = "COMFY_URL")]
    pub comfy_url: String,
}

pub struct ModelCheck {
    pub filename: String,
    pub available: Option<bool>,
}

pub struct Installed {
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

pub fn default_settings() -> Settings {
    let saved = data_dir().ok().and_then(|path| read_settings(&path));
    let comfy_root = environment_path("COMFY_ROOT")
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
    }
}

fn validate_comfy_url(value: &str) -> Result<String, String> {
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
    if !root.join("input").is_dir() || !root.join("output").is_dir() {
        return Err(
            "Choose the ComfyUI folder that contains both input and output directories.".into(),
        );
    }
    let comfy_root = root
        .to_str()
        .ok_or("The ComfyUI directory could not be represented as text.")?
        .into();
    let comfy_url = validate_comfy_url(&settings.comfy_url)?;
    Ok(Settings {
        comfy_root,
        comfy_url,
    })
}

fn fetch_json(agent: &ureq::Agent, origin: &str, endpoint: &str) -> Option<Value> {
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

pub fn probe(settings: &Settings) -> Result<Vec<ModelCheck>, String> {
    let settings = validate_settings(settings)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .into();
    let origin = &settings.comfy_url;
    if !fetch_json(&agent, origin, "/system_stats").is_some_and(|stats| stats["system"].is_object())
    {
        return Err("ComfyUI did not report system information. Start ComfyUI and check the URL, or finish setup and start it later.".into());
    }
    Ok(STARTER_MODELS
        .iter()
        .map(|&(node, field, filename)| ModelCheck {
            filename: filename.into(),
            available: fetch_json(&agent, origin, &format!("/object_info/{node}"))
                .and_then(|data| model_available(&data, node, field, filename)),
        })
        .collect())
}

fn quiet_command(program: impl AsRef<OsStr>) -> Command {
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

fn usable_python(program: impl AsRef<OsStr>, arguments: &[&str]) -> bool {
    quiet_command(program)
        .args(arguments)
        .args(["-c", PYTHON_VERSION_CHECK])
        .status()
        .is_ok_and(|status| status.success())
}

fn python_for_creation() -> Result<Command, String> {
    if let Some(program) = environment_path("RENDER_BRIDGE_PYTHON") {
        return usable_python(&program, &[])
            .then(|| quiet_command(&program))
            .ok_or(
                "RENDER_BRIDGE_PYTHON must name a working Python 3.11 or newer executable.".into(),
            );
    }
    let candidates: &[(&str, &[&str])] = if cfg!(windows) {
        &[("py", &["-3"]), ("python", &[]), ("python3", &[])]
    } else {
        &[("python3", &[]), ("python", &[])]
    };
    for &(program, arguments) in candidates {
        if usable_python(program, arguments) {
            let mut command = quiet_command(program);
            command.args(arguments);
            return Ok(command);
        }
    }
    Err("Python 3.11 or newer was not found. Install Python, reopen setup, or set RENDER_BRIDGE_PYTHON to its executable.".into())
}

fn venv_python(app_dir: &Path) -> PathBuf {
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

pub fn install(settings: &Settings, mut progress: impl FnMut(&str)) -> Result<Installed, String> {
    let settings = validate_settings(settings)?;
    let data_dir = data_dir()?;
    let app_dir = data_dir.join("app");
    let python = venv_python(&app_dir);
    let token_path = if let Some(path) = environment_path("MCP_TOKEN_FILE") {
        let path = if path.is_absolute() || path.starts_with("~") {
            path
        } else {
            app_dir.join(path)
        };
        resolve_path(&path).map_err(|_| "The configured MCP token path could not be resolved.")?
    } else {
        data_dir.join("token.txt")
    };
    let port = mcp_port()?;
    let installed = Installed {
        start_command: start_command(&data_dir, &app_dir, &python, &token_path, port)?,
        mcp_url: format!("http://127.0.0.1:{port}/mcp"),
        token_path,
        app_dir,
    };
    create_private_dir(&installed.app_dir)
        .map_err(|_| "Could not create the Render Bridge app directory.")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(installed.app_dir.join("install.lock"))
        .map_err(|_| "Could not open the Render Bridge install lock.")?;
    lock.try_lock().map_err(
        |_| "Could not lock the runtime for setup. Close any other setup wizard and retry.",
    )?;

    if !python.is_file() {
        progress("Looking for Python 3.11 or newer.");
        let mut command = python_for_creation()?;
        progress("Creating the Render Bridge virtual environment.");
        let status = command
            .args(["-m", "venv"])
            .arg(installed.app_dir.join(".venv"))
            .status()
            .map_err(|_| "Could not launch Python to create the virtual environment.")?;
        if !status.success() {
            return Err("Could not create the virtual environment. Check your Python installation and app directory permissions.".into());
        }
    }
    if !usable_python(&python, &[]) {
        return Err("The Render Bridge .venv needs Python 3.11 or newer. Move that environment aside and rerun setup.".into());
    }

    progress("Installing Python dependencies. This may take a minute.");
    let mut requirements = tempfile::Builder::new()
        .prefix(".requirements-")
        .suffix(".txt")
        .tempfile_in(&installed.app_dir)
        .map_err(|_| "Could not stage the Python requirements.")?;
    requirements
        .write_all(REQUIREMENTS_TEXT.as_bytes())
        .map_err(|_| "Could not write the Python requirements.")?;
    requirements
        .flush()
        .map_err(|_| "Could not write the Python requirements.")?;
    let status = quiet_command(&python)
        .args([
            "-m",
            "pip",
            "--disable-pip-version-check",
            "--no-input",
            "install",
            "-r",
        ])
        .arg(requirements.path())
        .status()
        .map_err(|_| "Could not launch pip from the Render Bridge virtual environment.")?;
    if !status.success() {
        return Err("Dependency installation failed. Check your package index and connection, then rerun setup.".into());
    }

    progress("Saving the runtime files and ComfyUI settings.");
    for (name, content) in RUNTIME_FILES {
        atomic_write(&installed.app_dir.join(name), content.as_bytes()).map_err(|_| {
            format!("Could not install {name}. Check the app directory permissions.")
        })?;
    }
    save_settings(&data_dir, &settings).map_err(|_| "Could not save the ComfyUI settings.")?;
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

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
    fn saving_backend_settings_preserves_existing_token_and_supports_retries() {
        let directory = tempfile::tempdir().unwrap();
        let token = directory.path().join("token.txt");
        fs::write(&token, b"fixture token; no real secret").unwrap();
        let settings = Settings {
            comfy_root: "fixture folder".into(),
            comfy_url: "http://localhost:8188".into(),
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
        })
        .unwrap();
        worker.join().unwrap();
        assert_eq!(
            checks
                .iter()
                .map(|check| check.available)
                .collect::<Vec<_>>(),
            vec![Some(true), Some(false), None, Some(true)],
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }
}
