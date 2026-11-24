# Render Bridge

A self-hosted image MCP server that lets your agents do the work in ComfyUI. Your agent can pick a preset, run a workflow, track the queue, and return the result as a signed download link. ComfyUI runs the pipeline on your own machine. Render Bridge is an independent integration by BlackRune LLC.

[Download for Windows x64](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/download/v0.1.0/RenderBridgeSetup.exe) · [Build the desktop installer](#build-the-desktop-installer) · [Set up your first pipeline](docs/pipelines.md#recommended-first-pipeline) · [Ask your agent to install it](#ask-your-agent-to-set-it-up)

## Install and run

1. Install and start [ComfyUI](https://docs.comfy.org/installation/manual_install) on the same host. Confirm its API responds at `http://127.0.0.1:8188/system_stats`. Find the ComfyUI directory that contains its `input` and `output` folders. For a Windows portable install, this is usually the inner `ComfyUI` directory.
2. Install Python 3.11 or later. Clone this repository or download its source, then open a terminal in that directory.

```sh
git clone https://github.com/BlackRune-LLC/render-mcp-bridge.git
cd render-mcp-bridge
```

### Desktop installer

[Download Render Bridge Setup v0.1.0 for Windows x64](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/download/v0.1.0/RenderBridgeSetup.exe). Open the downloaded executable after installing ComfyUI and Python 3.11 or later. The executable is unsigned. [Release notes, checksum, and third-party notices](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/tag/v0.1.0)

The Windows v0.1.0 download uses the original wizard. Build from the current source to use ComfyUI detection, **Start & Verify**, and the shortcut checkbox described below.

1. Start ComfyUI, then open the wizard. It checks the selected loopback address, standard ComfyUI addresses, and port hints from current Comfy Desktop builds. Choose your build if several are detected. If its paths cannot be confirmed from its launch settings, copy the actual port from the build's **Overview** and its input and output locations from **Folders**. [Comfy Desktop build settings](https://docs.comfy.org/installation/desktop/usage/instance-management)
2. Select **Check ComfyUI**. The wizard compares reported folders and checks starter model files. If the API cannot report absolute paths, as with a typical Windows portable launch, browse to the inner `ComfyUI` folder. Setup checks whether ComfyUI can read a temporary marker in each selected folder and removes the markers afterward. Open **Separate input and output folders** when your build uses different locations. If the wizard still cannot confirm the pairing, verify the paths manually or explicitly continue offline.
3. Select **Install Render Bridge**. This sets up the server and Python environment in the `app` folder inside your Render Bridge user data directory. Select **Create a Gaming Mode shortcut** before installing if you want one.
4. Select **Start & Verify** with **Claude Code**, **Codex**, or both checked. The wizard starts a local Bridge and makes an authenticated `server_status` call without rendering. It also checks the saved user settings for the selected clients. If you run Codex with a named profile, enter its name to check that file too. If an entry is missing, add it using the displayed endpoint and private token file, following [the client instructions](#connect-an-mcp-client). Select **Verify again**, then open a fresh client session to test that client's effective configuration.

Use **Open existing setup** to return to Connect without reinstalling. **Stop Bridge** or closing the wizard stops its Bridge session. To keep the Bridge running after closing setup, use **Shutdown and terminal commands** to copy the start command. Press **Ctrl+C** in that terminal when you want to stop it.

#### Build the desktop installer

Install a current [Rust toolchain](https://www.rust-lang.org/tools/install) and your platform's native build tools. Start the wizard from your checkout.

```sh
cargo run --locked --manifest-path installer/Cargo.toml
```

To build the standalone setup executable for the current platform, run `cargo build --release --locked --manifest-path installer/Cargo.toml`. The binary is in `installer/target/release/`. The packaged binary includes the bridge source and the pinned requirements. Installing those Python dependencies needs access to your configured package index.

**macOS.** Build on the target Mac using its native Rust toolchain. Install the Xcode Command Line Tools with `xcode-select --install` if needed. Launch the installer from Terminal so it inherits your Python path. If setup cannot find your Python, set `RENDER_BRIDGE_PYTHON` to the absolute executable path in that terminal before launching. For ComfyUI Desktop, choose the shared directory that contains `input` and `output`, typically `~/ComfyUI-Shared`. [Rust macOS requirements](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html) · [ComfyUI Desktop on macOS](https://docs.comfy.org/installation/desktop/macos)

**Linux.** Use your distro's native build packages. On Debian or Ubuntu, follow the [eframe build prerequisites](https://github.com/emilk/egui/blob/0.33.3/crates/eframe/README.md), install `libxcursor1` and `libxkbcommon-x11-0` for X11, and install `python3-venv` if you use the distro's Python and need virtual environment support. Run the wizard in an X11 or Wayland desktop session with OpenGL. The Browse button uses the desktop's file chooser portal. Install the portal backend for your desktop session if you need it, or enter the ComfyUI path directly. [Linux native dialogs](https://docs.rs/rfd/0.16.0/rfd/#xdg-desktop-portal-backend)

### Terminal installer

Use the wizard for a bridge and a ComfyUI API running on the same machine. It checks the data directory, the local API, and the starter model filenames. With your confirmation, it creates or updates the bridge's Python virtual environment and saves the ComfyUI path and URL in your user data directory. Use the model guide for any weights or nodes it flags as missing.

**Windows PowerShell**

```powershell
py -3 setup_render_bridge.py
py -3 setup_render_bridge.py start
```

**macOS or Linux**

```sh
python3 setup_render_bridge.py
python3 setup_render_bridge.py start
```

The `start` command uses the saved settings and runs the bridge in your terminal. Leave it running and follow [Connect an MCP client](#connect-an-mcp-client). The wizard prints the token file path so you can add the new entry while preserving your existing client settings.

### Manual setup

Follow the [model and node guide](docs/pipelines.md) to install the files for one pipeline. The [accounts section](docs/pipelines.md#accounts-and-credentials) explains when Hugging Face or a provider login is needed. Set `COMFY_ROOT` to the directory that owns the `input` and `output` folders, including for ComfyUI Desktop or a custom data location.

**Windows PowerShell**

```powershell
py -3 -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements.txt
$env:COMFY_ROOT = 'C:\path\to\ComfyUI'
.\.venv\Scripts\python.exe comfy_mcp_server.py
```

**macOS or Linux**

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements.txt
export COMFY_ROOT="/path/to/ComfyUI"
.venv/bin/python comfy_mcp_server.py
```

### Verify the server

Leave both processes running and check `http://127.0.0.1:8189/health`. Both `mcp` and `comfyui` should report `ok`. On first run, Render Bridge creates a bearer token. Its default path is `%LOCALAPPDATA%\comfy-mcp\token.txt` on Windows or `${XDG_DATA_HOME:-$HOME/.local/share}/comfy-mcp/token.txt` on macOS and Linux. Keep the token in the server's private token file and your MCP client's user-scope config.

| Setting | Default and purpose |
| --- | --- |
| `COMFY_ROOT` | `~/ComfyUI`. Set this to the actual ComfyUI data directory used for local file transfers. |
| `COMFY_INPUT_DIR`, `COMFY_OUTPUT_DIR` | Optional absolute paths for builds that use separate input or output folders. The defaults are `input` and `output` under `COMFY_ROOT`. |
| `COMFY_URL` | `http://127.0.0.1:8188`. Set this if your ComfyUI API uses a different port. |
| `MCP_HOST`, `MCP_PORT` | `127.0.0.1` and `8189`. |
| `COMFY_MCP_DATA_DIR` | Per-user data directory. Stores the token, gaming-mode state, and project records. |
| `PUBLIC_URLS` | Served URLs. Defaults to a loopback URL at the MCP port. For a remote client, use a named URL such as `private=https://mcp.example.com`. Signed uploads and downloads use this address. |

For remote access, keep the MCP server bound to loopback and put an HTTPS reverse proxy or private tunnel in front of it. Forward `/mcp`, `/files/`, and `/upload/`. For a reverse proxy, send the upstream `Host` header as `127.0.0.1:8189` (or the configured loopback port), stream MCP responses, and omit query strings from access logs. Set `PUBLIC_URLS` to the client-facing URL. Binding the MCP process itself to a non-loopback address requires the explicit `MCP_ALLOW_REMOTE=1` setting. Give the bearer token only to clients you trust with access to ComfyUI and your asset records.

## Connect an MCP client

Add a user-scope server named `comfyui` with the Streamable HTTP endpoint displayed by setup (default `http://127.0.0.1:8189/mcp`) and header `Authorization: Bearer <token>`. Start the Bridge first to create its token file. Replace `<token>` in your private client settings with that file's contents. Preserve any MCP entries you already use. Use another name if `comfyui` is taken.

For Claude Code, merge this entry into the top-level `mcpServers` object in `~/.claude.json` (`%USERPROFILE%\.claude.json` on Windows). For a custom profile, launch the wizard with that profile's `CLAUDE_CONFIG_DIR`. [Claude Code MCP scopes](https://code.claude.com/docs/en/mcp#user-scope)

```json
{
  "comfyui": {
    "type": "http",
    "url": "http://127.0.0.1:8189/mcp",
    "headers": { "Authorization": "Bearer <token>" }
  }
}
```

For Codex, add these tables to `~/.codex/config.toml` (`%USERPROFILE%\.codex\config.toml` on Windows). If you set `CODEX_HOME`, use `config.toml` in that directory and launch the wizard with the same setting. [Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)

```toml
[mcp_servers.comfyui]
url = "http://127.0.0.1:8189/mcp"
tool_timeout_sec = 600.0

[mcp_servers.comfyui.http_headers]
Authorization = "Bearer <token>"
```

Start a fresh client session and call `server_status` and `list_jobs`. If `/mcp` returns HTTP 401, check that this profile has the bearer token from Render Bridge's token file. If `/health` reports `comfyui: stopped`, start ComfyUI or check `COMFY_URL`.

## Make your first render

After installing the [Z-Image Turbo starter files](docs/pipelines.md#recommended-first-pipeline), try this in your MCP client.

> Call `server_status`, `list_jobs`, and `list_models` on Render Bridge. If the server is available and the `z_image_turbo` preset is installed, generate one 512 × 512 image of a small watercolor lighthouse and give me its signed download link. If a prerequisite is missing, tell me which one.

The [pipeline guide](docs/pipelines.md#what-each-pipeline-does) covers editing, background removal, animation, 3D, and workflows you export from ComfyUI. Use `start_job` and `get_job` for the generation tools that support background jobs.

## AI server gaming mode

If you selected **Create a Gaming Mode shortcut**, open **Render Bridge Gaming Mode** from the Windows or macOS desktop or from your Linux application menu. It toggles Gaming Mode and reports the new state. If you skipped the shortcut, go to **Connect → Shutdown and terminal commands → Turn Gaming Mode off** and run the copied command in a terminal. To turn it on or check its state, replace the final `off` with `on` or `status`. In the v0.1.0 download, copy the start command and replace the final `start` with `gaming-mode off`.

If you used the terminal installer, open a second terminal in the repo and use the same Python launcher you used for setup (`py -3` on Windows or `python3` on macOS and Linux).

```sh
python3 setup_render_bridge.py gaming-mode on
python3 setup_render_bridge.py gaming-mode status
python3 setup_render_bridge.py gaming-mode off
```

For manual setup, use `comfy_mcp_server.py gaming-mode on|off|status` with the virtual environment's Python and the same `COMFY_URL` and `COMFY_MCP_DATA_DIR` settings as the running server.

Turning gaming mode on pauses new GPU work through the MCP server. If ComfyUI's queue is idle, the command asks it to unload models and free memory. If it is busy, let the existing work finish and run `gaming-mode on` again to request the unload. The pause persists across restarts. Check `server_status` from your MCP client to confirm it, then switch gaming mode off when you're ready to generate again. Keep ComfyUI running separately.

## Ask your agent to set it up

Give the following prompt to an agent with terminal access on the machine where you want to run the server.

> Set up Render Bridge on this machine using the README and pipeline guide at https://github.com/BlackRune-LLC/render-mcp-bridge. Find my ComfyUI installation, check its API, and identify its data directory. If ComfyUI is missing, use its official installation guide. Walk me through the Rust desktop installer if I can use the GUI, the terminal installer if I can use an interactive shell, or manual setup if I need to run the steps separately. Register the server in the user-scope MCP config for the client I actually use. Preserve existing entries and keep tokens in private user settings. Never display tokens, put them on a command line, or ask me to send them in chat. Ask me to complete any required sign-in or gated-model agreement in my browser or terminal. Recommend one pipeline for my hardware, identify its exact model files and nodes, and ask before installing extra nodes, downloading weights, or generating anything. In a fresh client session, verify `server_status`, `list_jobs`, and `list_models`. Show me the launch, gaming-mode, and first-render commands.

## License

Render Bridge's source and documentation are MIT licensed. Selected graph builders adapt settings from Comfy Org's MIT-licensed [workflow templates](https://github.com/Comfy-Org/workflow_templates). ComfyUI is a separate application distributed under [GPL-3.0](https://github.com/Comfy-Org/ComfyUI/blob/master/LICENSE). Models and custom nodes have their own upstream terms.
