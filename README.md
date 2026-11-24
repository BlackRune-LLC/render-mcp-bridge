# Render Bridge

A self-hosted image MCP server that lets your agents do the work in ComfyUI. Your agent can pick a preset, run a workflow, track the queue, and return the result as a signed download link. ComfyUI runs the pipeline on your own machine. Render Bridge is an independent integration by BlackRune LLC.

[Download v0.1.0 for Windows x64](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/download/v0.1.0/RenderBridgeSetup.exe) · [Build the desktop installer](#build-the-desktop-installer) · [Windows control panel](#windows-control-panel) · [Connect an MCP client](#connect-an-mcp-client) · [Set up your first pipeline](docs/pipelines.md#recommended-first-pipeline) · [Ask your agent to set it up](#ask-your-agent-to-set-it-up)

## Install and run

Start here if Python, ComfyUI, and your agent client are all new to this machine. Render Bridge needs a full Python 3.11 or newer installation with virtual environment support, plus a local ComfyUI build on the same computer. Its standard setup runs without Docker. Allow internet access for the installers, Python packages, and any model weights you choose.

### First-time prerequisites

#### 1. Install Python for Render Bridge

Comfy Desktop and ComfyUI Portable manage Python for their own builds. Install a full Python distribution for Render Bridge so it can create its own virtual environment.

- **Windows.** If the wizard offers **Install Python with WinGet**, review its request to install Python for all users. Select **Request administrator approval** and follow the Windows prompts. WinGet selects the latest available Python 3.13 package from Microsoft's official feed. To install it yourself, choose **Get Python for Windows** or download the [official Python install manager](https://www.python.org/downloads/windows/). Run its installer, then open PowerShell and run `py install 3.13`. Open a fresh PowerShell window and check `py -3 --version` and `py -3 -c "import venv, ensurepip"`. [Using Python on Windows](https://docs.python.org/3/using/windows.html) · [WinGet install command](https://learn.microsoft.com/en-us/windows/package-manager/winget/install)
- **macOS.** Run the signed universal2 `.pkg` from the [Python macOS downloads page](https://www.python.org/downloads/macos/). Open a fresh Terminal window and check `python3 --version` and `python3 -c "import venv, ensurepip"`. [Using Python on macOS](https://docs.python.org/3/using/mac.html)
- **Linux.** Use your distribution's packages for Python and virtual environments. On Debian or Ubuntu, run `sudo apt update` and `sudo apt install python3 python3-venv`. Check `python3 --version` and `python3 -c "import venv, ensurepip"`. If the default Python is older than 3.11, look for a newer version and its matching `venv` package from your distribution, such as `python3.13` and `python3.13-venv`. Install them alongside the default Python and select the newer interpreter in the wizard. [Debian's virtual environment package](https://packages.debian.org/bookworm/python3-venv) · [Using Python on Linux](https://docs.python.org/3/using/unix.html)

#### 2. Install and start ComfyUI

Get [Comfy Desktop from Comfy Org](https://comfy.org/download), complete its first-run setup to install a **local build**, and wait for it to start. Check the hardware and free disk space shown in its setup screen. Comfy Desktop offers Windows x64, Windows ARM64 with NVIDIA, Apple silicon macOS, and Linux x64 installers. [Comfy Desktop setup](https://docs.comfy.org/installation/desktop/overview)

Use [ComfyUI Portable for Windows](https://docs.comfy.org/installation/comfyui_portable_windows) if you prefer a portable installation, or use [ComfyUI's manual setup](https://docs.comfy.org/installation/manual_install) for hardware or platforms that need another path. Follow ComfyUI's upstream instructions for your GPU, drivers, and launch command. Keep the local build running while using Render Bridge.

#### 3. Choose your agent client and first pipeline

Choose a client with authenticated Streamable HTTP support on the device where your agent will run. The connection guide below includes Claude Code, Codex, and VS Code Copilot. Complete any client sign-in in its own app or terminal. You can set up the Bridge and check its connection before downloading models. Before your first render, choose a starter pipeline and download its listed weights. [Client setup](docs/connections.md) · [Starter pipelines](docs/pipelines.md#recommended-first-pipeline) · [Accounts for gated models and hosted nodes](docs/pipelines.md#accounts-and-credentials)

### Desktop installer

[Download Render Bridge Setup v0.1.0 for Windows x64](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/download/v0.1.0/RenderBridgeSetup.exe). The executable is unsigned and uses the Microsoft Visual C++ x64 runtime. If the runtime is missing, install the [x64 Redistributable from Microsoft](https://aka.ms/vc14/vc_redist.x64.exe) before opening setup. [Release notes, checksum, and third-party notices](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/tag/v0.1.0)

The Windows v0.1.0 download uses the original wizard. Complete the prerequisites above before opening it. Build the installer from current source for the following setup flow and the compact Windows control panel.

1. Open the wizard. On **Prerequisites**, check Python or follow the official download links. After installing Python, choose **Check Python again**. If it still isn't detected, use **Select Python executable** or reopen the wizard so it sees your updated PATH. Install and start a local ComfyUI build, then continue. On the **ComfyUI** screen, choose your build if several are detected. If its paths cannot be confirmed from its launch settings, copy the port from the build's **Overview** and its input and output locations from **Folders**. [Comfy Desktop build settings](https://docs.comfy.org/installation/desktop/usage/instance-management)
2. Select **Check ComfyUI**. The wizard compares reported folders and checks starter model files. If the API cannot report absolute paths, as with a typical Windows portable launch, browse to the inner `ComfyUI` folder. Setup checks whether ComfyUI can read a temporary marker in each selected folder and removes the markers afterward. Open **Separate input and output folders** when your build uses different locations. If the wizard still cannot confirm the pairing, verify the paths manually or explicitly continue offline.
3. Review the starter pipelines on **Models**. You can choose a separate model drive and copy the settings ComfyUI needs to find it. On **Install**, keep the default Render Bridge installation folder or choose another one. Select **Install Render Bridge** to set up the server and its Python environment. Setup checks a new runtime before activating it. If an update fails or is cancelled, the previous runtime stays usable with the same token. Select **Create a Gaming Mode shortcut** before installing if you want one. [Store models on another drive](docs/pipelines.md#store-models-on-another-drive)
4. On **Connect**, select **Start & Verify**, then **Copy agent setup prompt**. Paste it into the AI agent that will connect on this device. The prompt gives the agent the exact local address and the path to the private token file. Approve any requested user configuration edits, then let the agent verify the connection with `server_status`. Open a fresh client session if it needs to reload the configuration. [Connect an MCP client](#connect-an-mcp-client)

**Start & Verify** also checks saved configuration for selected Claude Code or Codex clients. Enter your named Codex profile if you use one.

Use **Open existing setup** on **Prerequisites** to return to **Connect**. If you installed in a custom location, use **Browse for an installation** and select its Render Bridge installation folder. To upgrade a v0.1.0 installation, go through **Install Render Bridge** again with the same user data directory and ComfyUI build. The installer prepares the updated runtime and keeps your existing token. Open **ComfyUI Jobs** from **ComfyUI** or **Connect** to check the selected build's queue inside the wizard.

When you choose **Stop Bridge** or close a wizard that started the Bridge, review the queue before confirming. Choose **Keep Bridge running** to wait or **Stop Bridge** to confirm. The confirmation reads **Stop Bridge anyway** when work is running or queued, or when the queue is unavailable. ComfyUI runs separately and has its own shutdown controls.

To keep the Bridge running after closing setup, use **Shutdown and terminal commands** to copy the start command. Press **Ctrl+C** in that terminal when you want to stop it.

#### Windows control panel

In the installer built from current source, choose **Open Render Bridge** on **Connect** to launch the compact panel. It also has a per-user **Render Bridge** entry in the Windows Start menu. Dark is the default. In **Settings**, choose **Light**, **Dark**, or **System** to follow Windows appearance.

- **Jobs** shows running and queued jobs for the local ComfyUI address selected in setup. When ComfyUI sends progress for a running stage, the panel shows its step count and percentage.
- **Start server** starts Render Bridge using your saved installation. When the server is running, the muted button remains clickable to confirm its status. The red **Stop server** button stops the session started by this panel after you review the queue and confirm. Manage ComfyUI in its own app or through its launch command.
- **Hide to tray** keeps Render Bridge in the Windows notification area. Click its icon to reopen the panel. If the icon is in the overflow, open **Show hidden icons** first.
- Select **Connect** to reopen **Connect an agent**, copy connection details, or rotate the token after stopping the server. [Connect after installation](docs/connections.md#connect-after-installation)
- In **Settings**, turn on **Start hidden when I sign in** to open Render Bridge in the notification area after you sign in to Windows. Use **Start server** in the panel when you want the Render Bridge MCP server to run.
- Select **Update** to check for a newer stable Windows release on the official GitHub repository. If that release has a signed installer, choose **Install update** to download and verify it from the panel. Render Bridge checks for active jobs and any server session the panel owns before replacing the installed version. The update keeps your saved settings, ComfyUI address, and token on this PC.

#### Storage and configuration

Choose an optional generated asset folder, backup folder, or both during installation. In the Windows panel, open **Settings → Storage** to change those choices. Select **Copy generated assets** when you want a copy of the configured ComfyUI output. The copy keeps subfolders, preserves the original files, and leaves existing destination files in place.

Use **Settings → Advanced configuration → Open configuration file** to review the saved ComfyUI data root, URL, and any separate input or output paths. Stop Render Bridge before editing the file, then restart it and verify the connection. Set runtime tuning variables in the environment that starts Render Bridge. Render Bridge requests a model unload after 10 minutes of idle time by default. Set `IDLE_UNLOAD_MIN=0` to disable idle unloading. [Configuration reference](docs/configuration.md)

#### Build the desktop installer

Install a current [Rust toolchain](https://www.rust-lang.org/tools/install) and your platform's native build tools. Download and extract the source archive or use [Git](https://git-scm.com/downloads) to clone the repository. Open a terminal in the extracted folder or checkout and start the wizard.

```sh
cargo run --locked --manifest-path installer/Cargo.toml
```

On Windows x64, use `./installer/build-windows.ps1` from PowerShell to build the standalone release executable at `installer/target/x86_64-pc-windows-msvc/release/render-bridge-setup.exe`. On macOS or Linux, run `cargo build --release --locked --manifest-path installer/Cargo.toml` and find the binary in `installer/target/release/`. The packaged binary includes the bridge source and the pinned requirements. Installing those Python dependencies needs access to your configured package index.

Windows x64 has a downloadable installer. macOS and Linux use the native source build instructions below. See the [release checklist](docs/releases.md) for signing and clean-machine verification.

**Windows source builds.** Install the MSVC C++ build tools and a Windows SDK alongside the x64 MSVC Rust toolchain. [Rust's Windows build prerequisites](https://rust-lang.github.io/rustup/installation/windows-msvc.html)

**macOS.** Build on the target Mac using its native Rust toolchain. Install the Xcode Command Line Tools with `xcode-select --install` if needed. Launch the installer from Terminal so it inherits your Python path. If setup cannot find your Python, set `RENDER_BRIDGE_PYTHON` to the absolute executable path in that terminal before launching. For ComfyUI Desktop, choose the shared directory that contains `input` and `output`, typically `~/ComfyUI-Shared`. [Rust macOS requirements](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html) · [ComfyUI Desktop on macOS](https://docs.comfy.org/installation/desktop/macos)

**Linux.** Use your distro's native build packages. On Debian or Ubuntu, follow the [eframe build prerequisites](https://github.com/emilk/egui/blob/0.33.3/crates/eframe/README.md). On a minimal X11 install, check for `libxcursor1`, `libxi6`, `libxrandr2`, and `libxkbcommon-x11-0`. Install `python3-venv` if you use the distro's Python and need virtual environment support. Run the wizard in an X11 or Wayland desktop session with OpenGL. The Browse button uses the desktop's file chooser portal. Install the portal backend for your desktop session if you need it, or enter the ComfyUI path directly. [Linux native dialogs](https://docs.rs/rfd/0.16.0/rfd/#xdg-desktop-portal-backend)

### Terminal installer

For terminal setup, finish the [first-time prerequisites](#first-time-prerequisites), then download and extract the source archive or clone the repository. Run the commands below in the source folder. The terminal setup checks the data directory, the local API, and the starter model filenames. With your confirmation, it creates or updates the bridge's Python virtual environment and saves the ComfyUI path and URL in your user data directory. Use the model guide for any weights or nodes it flags as missing.

For a ComfyUI build with separate folders, set `COMFY_INPUT_DIR` and `COMFY_OUTPUT_DIR` to their absolute paths before running `setup_render_bridge.py install` so setup can check and save them.

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

The `start` command uses the saved settings and runs the Bridge in your terminal. Setup prints the private token file path. Leave the server running and use the [copy-and-paste connection prompt](docs/connections.md#paste-a-connection-prompt).

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
| `PUBLIC_URLS` | Comma-separated `name=base-url` entries for signed files, such as `private=https://render.example.com`. Defaults to loopback at the MCP port. |

## Connect an MCP client

Use an agent client that supports **Streamable HTTP** and a bearer token or custom HTTP header. Render Bridge and ComfyUI run together on the render host. The agent can run on that host or on another device with a network route to Render Bridge.

1. On **Connect** in the current Windows installer, start Render Bridge and select **Copy agent setup prompt**. After installation, use **Connect** in the Render Bridge panel to copy the prompt again. If the server is offline, select **Start server** first. Paste the prompt into the client on the render host.
2. For terminal setup, use the [copy-and-paste connection prompt](docs/connections.md#paste-a-connection-prompt). For a client on another device, set up its [private connection and credentials](docs/connections.md#connect-from-another-device) first.
3. Verify with `server_status` in the intended client session. If the client does not reload its MCP configuration, reopen it and run the check there.

For manual setup, follow the [client examples](docs/connections.md#configure-the-client) for Claude Code, Codex, VS Code Copilot, or another compatible client.

## Make your first render

After installing the [Z-Image Turbo starter files](docs/pipelines.md#recommended-first-pipeline), try this in your MCP client.

> Call `server_status`, `list_jobs`, and `list_models` on Render Bridge. If the server is available and the `z_image_turbo` preset is installed, generate one 512 × 512 image of a small watercolor lighthouse and give me its signed download link. If a prerequisite is missing, tell me which one.

The [pipeline guide](docs/pipelines.md#what-each-pipeline-does) covers editing, background removal, animation, 3D, and workflows you export from ComfyUI. Use `start_job` and `get_job` for the generation tools that support background jobs.

## Render Bridge Gaming Mode

Select **Gaming Mode · Off** in the Jobs panel or on **Connect** to pause new GPU work. The controller button turns bright green and reads **Gaming Mode · On** while the pause is active. Select it again to resume. If you selected **Create a Gaming Mode shortcut**, open **Render Bridge Gaming Mode** from the Windows or macOS desktop or from your Linux application menu. If you have an older shortcut that toggles Gaming Mode immediately, rename or remove it before creating the new shortcut.

For terminal control, go to **Connect → Shutdown and terminal commands → Turn Gaming Mode off** and run the copied command. Use **Copy status command** to check it in a terminal or replace the final `off` with `on` to enable it. In the v0.1.0 download, copy the start command and replace the final `start` with `gaming-mode off`.

If you used the terminal installer, open a second terminal in the repo and use the same Python launcher you used for setup (`py -3` on Windows or `python3` on macOS and Linux).

```sh
python3 setup_render_bridge.py gaming-mode on
python3 setup_render_bridge.py gaming-mode status
python3 setup_render_bridge.py gaming-mode off
```

For manual setup, use `comfy_mcp_server.py gaming-mode on|off|status` with the virtual environment's Python and the same `COMFY_URL` and `COMFY_MCP_DATA_DIR` settings as the running server.

Turning gaming mode on pauses new GPU work through the MCP server. If ComfyUI's queue is idle, the command asks it to unload models and free memory. If it is busy, let the existing work finish and run `gaming-mode on` again to request the unload. The pause persists across restarts. Check `server_status` from your MCP client to confirm it, then switch gaming mode off when you're ready to generate again. Keep ComfyUI running separately.

## Ask your agent to set it up

If Render Bridge is already installed, use the [short connection prompt](docs/connections.md#paste-a-connection-prompt). For a first-time installation, give the following prompt to an agent with terminal access on the render host. [Agent instructions](AGENTS.md)

> Set up Render Bridge on this machine using the README, connection guide, and pipeline guide at https://github.com/BlackRune-LLC/render-mcp-bridge. Start with its first-time prerequisites. Check for Python 3.11 or newer with virtual environment support. If it is missing, guide me through the official Python installer or distribution packages for my OS. Ask before starting a machine-wide install and let me approve any operating system prompt myself. Find my local ComfyUI build, check its API, and identify its data directory. If it is missing, help me choose and start a local build using the official ComfyUI instructions. Walk me through the Rust desktop wizard, the terminal installer, or manual setup according to the tools available. Register the server in the user-scope MCP config for the client I use. If the client runs on another device, follow the private-network setup in the connection guide for both MCP and signed file transfers. Preserve existing entries and keep tokens in private user settings. Never display tokens, put them on a command line, or ask me to send them in chat. Ask me to complete any required client sign-in or gated-model agreement in my own app, browser, or terminal. Recommend one pipeline for my hardware, identify its exact model files and nodes, and ask before installing extra nodes, downloading weights, or generating anything. In a fresh client session, verify `server_status`, `list_jobs`, and `list_models`. Show me the launch, gaming-mode, and first-render commands.

## License

Render Bridge's source and documentation are MIT licensed. Selected graph builders adapt settings from Comfy Org's MIT-licensed [workflow templates](https://github.com/Comfy-Org/workflow_templates). ComfyUI is a separate application distributed under [GPL-3.0](https://github.com/Comfy-Org/ComfyUI/blob/master/LICENSE). Models and custom nodes have their own upstream terms.
