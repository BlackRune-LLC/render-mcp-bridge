use crate::setup::{self, Installed};
use std::io::ErrorKind;
use std::net::{Ipv4Addr, TcpListener};
use std::process::{Child, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
pub enum Client {
    Claude,
    Codex,
}

impl Client {
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

pub struct RunningBridge {
    child: Child,
    marker: tempfile::NamedTempFile,
    settings: setup::Settings,
}

impl RunningBridge {
    fn start(installed: &Installed, settings: &setup::Settings) -> Result<Self, String> {
        let port = available_port(installed)?;
        let marker = tempfile::NamedTempFile::new_in(&installed.app_dir)
            .map_err(|_| "Could not prepare the Bridge startup check.")?;
        let id = marker
            .path()
            .file_name()
            .ok_or("Could not prepare a Bridge session ID.")?;
        let child = setup::quiet_command(setup::venv_python(&installed.app_dir))
            .arg(installed.app_dir.join("comfy_mcp_server.py"))
            .current_dir(&installed.app_dir)
            .stdin(Stdio::piped())
            .env("COMFY_MCP_DATA_DIR", &installed.data_dir)
            .env("COMFY_ROOT", &settings.comfy_root)
            .env("COMFY_URL", &settings.comfy_url)
            .env("COMFY_INPUT_DIR", setup::folder(settings, "input"))
            .env("COMFY_OUTPUT_DIR", setup::folder(settings, "output"))
            .env("MCP_HOST", "127.0.0.1")
            .env("MCP_PORT", port.to_string())
            .env("MCP_TOKEN_FILE", &installed.token_path)
            .env("RENDER_BRIDGE_MANAGED_ID", id)
            .env("RENDER_BRIDGE_PANEL_MANAGED", if cfg!(windows) { "1" } else { "0" })
            .spawn()
            .map_err(|_| "Could not start Render Bridge. Run the copied start command in a terminal for details.")?;
        Ok(Self {
            child,
            marker,
            settings: settings.clone(),
        })
    }

    pub fn id(&self) -> String {
        self.marker
            .path()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn backend_url(&self) -> &str {
        &self.settings.comfy_url
    }
}

impl Drop for RunningBridge {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.is_running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(40));
        }
        if self.is_running() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn origin(installed: &Installed) -> Result<&str, String> {
    installed
        .mcp_url
        .strip_suffix("/mcp")
        .ok_or_else(|| "The Bridge MCP URL is invalid.".into())
}

fn available_port(installed: &Installed) -> Result<u16, String> {
    let port = url::Url::parse(origin(installed)?)
        .ok()
        .and_then(|url| url.port_or_known_default())
        .ok_or("The Bridge port is missing.")?;
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|error| {
        if error.kind() == ErrorKind::AddrInUse {
            format!("Port {port} is already in use. Stop the other listener or reopen setup with a different MCP_PORT.")
        } else {
            format!("Could not listen on port {port}. Check your permissions or reopen setup with a different MCP_PORT.")
        }
    })?;
    Ok(port)
}

pub fn bridge_is_listening(installed: &Installed) -> bool {
    let Ok(origin) = origin(installed).and_then(setup::validate_comfy_url) else {
        return false;
    };
    let agent = setup::loopback_agent(Duration::from_secs(4));
    setup::fetch_json(&agent, &origin, "/health").is_some_and(|health| {
        health["mcp"] == "ok"
            && matches!(
                health["comfyui"].as_str(),
                Some("ok" | "stopped" | "paused")
            )
    })
}

pub fn start_and_verify(
    installed: &Installed,
    running: &mut Option<RunningBridge>,
    clients: &[Client],
    codex_profile: &str,
) -> Result<serde_json::Value, String> {
    validate_client_options(clients, codex_profile)?;
    let settings = setup::saved_install_settings(installed)?;
    let paired = match setup::recheck_folders(&settings) {
        Ok(paired) => paired,
        Err(message) => {
            *running = None;
            return Err(message);
        }
    };
    if !running
        .as_mut()
        .is_some_and(|bridge| bridge.settings == settings && bridge.is_running())
    {
        *running = None;
        *running = Some(RunningBridge::start(installed, &settings)?);
    }
    let bridge = running.as_ref().ok_or("Render Bridge did not start.")?;
    let mut result =
        verify_started_child(installed, &settings, &bridge.id(), clients, codex_profile)?;
    result["folders"] = paired.into();
    Ok(result)
}

#[cfg(windows)]
pub fn start_installed_and_verify(
    installed: &Installed,
    clients: &[Client],
    codex_profile: &str,
) -> Result<serde_json::Value, String> {
    validate_client_options(clients, codex_profile)?;
    let settings = setup::saved_install_settings(installed)?;
    let paired = setup::recheck_folders(&settings)?;
    if !bridge_is_listening(installed) {
        available_port(installed)?;
        // Detect old verifier arguments before starting an outdated runtime in the tray.
        run_verifier(installed, &settings, &[], "")?;
        crate::shortcuts::start_monitor(installed)?;
        let deadline = Instant::now() + Duration::from_secs(24);
        while !bridge_is_listening(installed) {
            if Instant::now() >= deadline {
                return Err("The installed panel could not start Render Bridge. Open the panel to check the server status.".into());
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
    let mut result = verify_listener(installed, clients, codex_profile)?;
    result["folders"] = paired.into();
    Ok(result)
}

fn validate_client_options(clients: &[Client], codex_profile: &str) -> Result<(), String> {
    if clients.contains(&Client::Codex)
        && (codex_profile.len() > 80
            || !codex_profile
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')))
    {
        return Err(
            "Use letters, numbers, hyphens, or underscores in the Codex profile name.".into(),
        );
    }
    Ok(())
}

fn verify_started_child(
    installed: &Installed,
    settings: &setup::Settings,
    id: &str,
    clients: &[Client],
    codex_profile: &str,
) -> Result<serde_json::Value, String> {
    let origin = origin(installed)?;
    let agent = setup::loopback_agent(Duration::from_secs(4));
    let deadline = Instant::now() + Duration::from_secs(14);
    loop {
        let result = setup::fetch_json(&agent, origin, "/health");
        if result
            .as_ref()
            .and_then(|health| health["wizard_session"].as_str())
            == Some(id)
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err("The wizard could not confirm its Bridge is listening. Check the port or run the copied start command in a terminal for details.".into());
        }
        thread::sleep(Duration::from_millis(200));
    }
    run_verifier(installed, settings, clients, codex_profile)
}

pub fn verify_listener(
    installed: &Installed,
    clients: &[Client],
    codex_profile: &str,
) -> Result<serde_json::Value, String> {
    validate_client_options(clients, codex_profile)?;
    let settings = setup::saved_install_settings(installed)?;
    run_verifier(installed, &settings, clients, codex_profile)
}

fn run_verifier(
    installed: &Installed,
    settings: &setup::Settings,
    clients: &[Client],
    codex_profile: &str,
) -> Result<serde_json::Value, String> {
    let backend = setup::validate_comfy_url(&settings.comfy_url)?;
    let mut command = setup::quiet_command(setup::venv_python(&installed.app_dir));
    command
        .arg(installed.app_dir.join("verify_render_bridge.py"))
        .arg(&installed.mcp_url)
        .arg(&installed.token_path)
        .args(["--require-owner", "--expected-backend", &backend])
        .arg("--expected-folders")
        .arg(setup::folder(settings, "input"))
        .arg(setup::folder(settings, "output"));
    if clients.contains(&Client::Codex) && !codex_profile.is_empty() {
        command.args(["--codex-profile", codex_profile]);
    }
    let output = command
        .args(clients.iter().map(|client| client.key()))
        .current_dir(&installed.app_dir)
        .stdout(Stdio::piped())
        .output()
        .map_err(|_| "The connection check could not start.")?;
    if !output.status.success() {
        if output.status.code() == Some(2) {
            return Err("Repair this installation with the current setup before comparing server ownership.".into());
        }
        return Err("The connection check exited unexpectedly. Reopen setup and try again.".into());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "The connection check did not return a result.".into())
}

pub fn profile_path(client: Client) -> Option<std::path::PathBuf> {
    let (variable, subdir, file) = match client {
        Client::Claude => ("CLAUDE_CONFIG_DIR", "", ".claude.json"),
        Client::Codex => ("CODEX_HOME", ".codex", "config.toml"),
    };
    let root = std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(subdir)))?;
    Some(root.join(file))
}
