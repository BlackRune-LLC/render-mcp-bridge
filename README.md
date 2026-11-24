# Render Bridge

A self-hosted image MCP server that lets your agents do the work in ComfyUI. Your agent can pick a preset, run a workflow, track the queue, and return the result as a signed download link. ComfyUI runs the pipeline on your own machine. Render Bridge is an independent integration by BlackRune LLC.

Render Bridge runs locally and sends no telemetry or crash reports to BlackRune. [Privacy and network use](PRIVACY.md)

[Release notes](CHANGELOG.md) · [Support](#support)

[Download for Windows x64 (unsigned)](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/latest/download/RenderBridgeSetup.exe) · [Install from source](#install-from-source) · [Ask your agent to set it up](#ask-your-agent-to-set-it-up) · [Windows control panel](#windows-control-panel) · [Connect an MCP client](#connect-an-mcp-client) · [Set up your first pipeline](docs/pipelines.md#recommended-first-pipeline)

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

[Download Render Bridge Setup for Windows x64](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/latest/download/RenderBridgeSetup.exe). The executable is unsigned. Verify it against the published SHA-256 before opening setup. [Release notes, checksum, and third-party notices](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/latest)

If you are using v0.1.0, complete the prerequisites before opening its original wizard. If that version reports a missing Visual C++ runtime, install the [x64 Redistributable from Microsoft](https://aka.ms/vc14/vc_redist.x64.exe). The steps below describe the current wizard.

1. Open the wizard. On **Prerequisites**, check Python or follow the official download links. After installing Python, choose **Check Python again**. If it still isn't detected, use **Select Python executable** or reopen the wizard so it sees your updated PATH. Install and start a local ComfyUI build, then continue. On the **ComfyUI** screen, choose your build if several are detected. If its paths cannot be confirmed from its launch settings, copy the port from the build's **Overview** and its input and output locations from **Folders**. [Comfy Desktop build settings](https://docs.comfy.org/installation/desktop/usage/instance-management)
2. Select **Check ComfyUI**. The wizard compares reported folders and checks starter model files. If the API cannot report absolute paths, as with a typical Windows portable launch, browse to the inner `ComfyUI` folder. Setup checks whether ComfyUI can read a temporary marker in each selected folder and removes the markers afterward. Open **Separate input and output folders** when your build uses different locations. If the wizard still cannot confirm the pairing, verify the paths manually or explicitly continue offline.
3. On **Models**, choose a pack and select **Download missing files**. Setup shows the download size and model folder, then refreshes the active library. You can also download packs later. On **Install**, choose the Render Bridge installation folder and select **Install Render Bridge** to set up its runtime. If an update fails or is cancelled, the previous runtime stays usable with the same token. Select **Create a Gaming Mode shortcut** if you want one. [Download models](docs/pipelines.md#download-models-in-render-bridge)
4. On **Connect**, select **Start & Verify**, then **Copy agent setup prompt**. Paste it into the agent you want to connect. If it needs to reload its configuration, open a fresh client session. After downloading the starter pack, use **First render** to try a test from your agent. On Windows, choose **Finish and keep running** to close setup and leave the installed panel in charge of your server. [Connect an MCP client](#connect-an-mcp-client)

**Start & Verify** checks the authenticated MCP connection, the selected ComfyUI build, and saved settings for selected Claude Code or Codex clients. Enter your named Codex profile if you use one. After downloading the first model pack, verify that your agent can call the tools and retrieve an output.

For a first-render test in setup, copy **First render**'s test prompt into the connected agent. After the test, paste only its signed PNG link into **Verify test PNG**. Setup checks that the file came from this installation and opens as a 512 × 512 PNG.

Use **Open existing setup** on **Prerequisites** to return to **Connect**. If you installed in a custom location, use **Browse for an installation** and select its Render Bridge installation folder. To upgrade a v0.1.0 installation, go through **Install Render Bridge** again with the same user data directory and ComfyUI build. The installer prepares the updated runtime and keeps your existing token. Open **ComfyUI Jobs** from **ComfyUI** or **Connect** to check the selected build's queue inside the wizard.

On Windows, open **Render Bridge** and use **Stop server** when you are ready to stop the server. On macOS and Linux, the wizard reviews the queue before it stops a server that it started. Confirm the stop or wait for active work to finish. ComfyUI runs separately and has its own shutdown controls.

To keep the Bridge running after closing setup, use **Shutdown and terminal commands** to copy the start command. Press **Ctrl+C** in that terminal when you want to stop it.

#### Windows control panel

In the installer built from current source, choose **Open Render Bridge** on **Connect** to launch the compact panel. It also has a per-user **Render Bridge** entry in the Windows Start menu. **Settings** opens in a separate resizable window. Dark is the default. Choose **Light**, **Dark**, or **System** to follow Windows appearance.

- **Jobs** shows running and queued jobs for the local ComfyUI address selected in setup. When ComfyUI sends progress for a running stage, the panel shows its step count and percentage.
- **Models** has **Installed**, **Packs**, and **Downloads**. Search the installed library or choose a pack and review its size and destination. Render Bridge refreshes the list after a download. [Download models](docs/pipelines.md#download-models-in-render-bridge)
- **Pipelines** previews model swaps for Wan Animate 2. Validate the requirements, save a local revision, and copy the run option for your agent. **Nodes** shows what the selected ComfyUI build reports as installed. [Review pipelines and nodes](docs/pipelines.md#review-pipelines-and-nodes)
- **Start server** starts Render Bridge using your saved installation. When the server is running, the muted button remains clickable to confirm its status. The red **Stop server** button stops the session started by this panel after you review the queue and confirm. Manage ComfyUI in its own app or through its launch command.
- **Hide to tray** keeps Render Bridge in the Windows notification area. Click its icon to reopen the panel. If the icon is in the overflow, open **Show hidden icons** first.
- Select **Connect** to reopen **Connect an agent**, copy connection details, or rotate the token after stopping the server. [Connect after installation](docs/connections.md#connect-after-installation)
- In **Settings**, turn on **Start hidden when I sign in** to open Render Bridge in the notification area after you sign in to Windows. **Custom theme** edits, imports, and exports color palettes. Use **Start server** in the panel when you want the Render Bridge MCP server to run. [Theme plugins](docs/configuration.md#theme-plugins)
- Select **Update** to check the official GitHub repository for a stable Windows build. When a build is available, select **Open GitHub download** and follow the manual update steps. The panel also detects a published build that differs from your preview, even if both have the same version number. [Update manually on Windows](#update-manually-on-windows)

#### Update manually on Windows

1. Download `RenderBridgeSetup.exe` and its SHA-256 checksum from the [official GitHub release](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/latest). Windows releases are unsigned. In PowerShell, run `Get-FileHash -Algorithm SHA256 .\RenderBridgeSetup.exe` in your download folder and compare the result with the published checksum before opening setup.
2. Let active jobs finish, stop the Render Bridge server, and exit the Render Bridge app from its notification-area menu.
3. Run setup, select the same ComfyUI build and Render Bridge installation folder, and choose **Install Render Bridge**. Reopen the panel and check the new version and agent connection. The update keeps your settings, ComfyUI address, and token.

The optional **Install signed build** action requires a valid Windows signature and checks the download and queue before starting the installer.

#### Storage and configuration

Choose an optional generated asset folder, backup folder, or both during installation. In the Windows panel, open **Settings → Storage** to change those choices. Select **Copy generated assets** when you want a copy of the configured ComfyUI output. The copy keeps subfolders, preserves the original files, and leaves existing destination files in place.

Use **Settings → Advanced configuration → Open configuration file** to review the saved ComfyUI data root, URL, and any separate input or output paths. Stop Render Bridge before editing the file, then restart it and verify the connection. Set runtime tuning variables in the environment that starts Render Bridge. Render Bridge requests a model unload after 10 minutes of idle time by default. Set `IDLE_UNLOAD_MIN=0` to disable idle unloading. [Configuration reference](docs/configuration.md)

Use **Settings → Open crash logs** to find local Render Bridge crash reports on Windows. [Local crash logs](docs/configuration.md#local-crash-logs)

To repair or uninstall an existing Windows setup, run the setup wizard again. [Windows installation, repair, and removal](docs/configuration.md#windows-installation-repair-and-removal)

#### Build the desktop installer

Install a current [Rust toolchain](https://www.rust-lang.org/tools/install) and your platform's native build tools. Download and extract the source archive or use [Git](https://git-scm.com/downloads) to clone the repository. Open a terminal in the extracted folder or checkout and start the wizard.

```sh
cargo run --locked --manifest-path installer/Cargo.toml
```

On Windows x64, use `./installer/build-windows.ps1` from PowerShell to build the standalone release executable at `installer/target/x86_64-pc-windows-msvc/release/render-bridge-setup.exe`. On macOS or Linux, run `cargo build --release --locked --manifest-path installer/Cargo.toml` and find the binary in `installer/target/release/`. The packaged binary includes the bridge source and the pinned requirements. Installing those Python dependencies needs access to your configured package index.

Windows x64 has a downloadable installer. macOS and Linux use the native source build instructions below. See the [release checklist](docs/releases.md) for checksums and clean-machine verification.

**Windows source builds.** Install the MSVC C++ build tools and a Windows SDK alongside the x64 MSVC Rust toolchain. [Rust's Windows build prerequisites](https://rust-lang.github.io/rustup/installation/windows-msvc.html)

**macOS.** Build on the target Mac using its native Rust toolchain. Install the Xcode Command Line Tools with `xcode-select --install` if needed. Launch the installer from Terminal so it inherits your Python path. If setup cannot find your Python, set `RENDER_BRIDGE_PYTHON` to the absolute executable path in that terminal before launching. For ComfyUI Desktop, choose the shared directory that contains `input` and `output`, typically `~/ComfyUI-Shared`. [Rust macOS requirements](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html) · [ComfyUI Desktop on macOS](https://docs.comfy.org/installation/desktop/macos)

**Linux.** Use your distro's native build packages. On Debian or Ubuntu, follow the [eframe build prerequisites](https://github.com/emilk/egui/blob/0.33.3/crates/eframe/README.md). On a minimal X11 install, check for `libxcursor1`, `libxi6`, `libxrandr2`, and `libxkbcommon-x11-0`. Install `python3-venv` if you use the distro's Python and need virtual environment support. Run the wizard in an X11 or Wayland desktop session with OpenGL. The Browse button uses the desktop's file chooser portal. Install the portal backend for your desktop session if you need it, or enter the ComfyUI path directly. [Linux native dialogs](https://docs.rs/rfd/0.16.0/rfd/#xdg-desktop-portal-backend)

### Install from source

Install [Git](https://git-scm.com/downloads), complete the [first-time prerequisites](#first-time-prerequisites), and start a local ComfyUI build. The terminal installer creates its own Python virtual environment with the dependencies from the repository. Rust is needed only to [build the desktop installer](#build-the-desktop-installer).

In setup, enter the directory that owns the selected ComfyUI build's `input` and `output` folders and its local API address. The usual API address is `http://127.0.0.1:8188`. If the build uses separate folders, set `COMFY_INPUT_DIR` and `COMFY_OUTPUT_DIR` to their absolute paths before running the install command. [Configuration reference](docs/configuration.md#terminal-and-server-options)

**Windows PowerShell**

```powershell
git clone https://github.com/BlackRune-LLC/render-mcp-bridge.git
Set-Location render-mcp-bridge
py -3 .\setup_render_bridge.py install
```

After setup finishes, start the server in the same folder.

```powershell
py -3 .\setup_render_bridge.py start
```

**macOS or Linux**

```sh
git clone https://github.com/BlackRune-LLC/render-mcp-bridge.git
cd render-mcp-bridge
python3 setup_render_bridge.py install
```

After setup finishes, start the server in the same folder.

```sh
python3 setup_render_bridge.py start
```

If you already have a checkout, run the install and start commands from it. When using a custom Render Bridge data folder, pass `--data-dir` with the same folder before `install` and `start`. Keep the server terminal open while your agent uses Render Bridge. Use the private token file path printed by setup to [connect your client](docs/connections.md#paste-a-connection-prompt).

### Manual setup without the setup script

From the source folder, create a virtual environment and install the Python requirements. Replace the ComfyUI path and API URL with the values for your running local build.

**Windows PowerShell**

```powershell
py -3 -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements.txt
$env:COMFY_ROOT = 'C:\path\to\ComfyUI'
$env:COMFY_URL = 'http://127.0.0.1:8188'
.\.venv\Scripts\python.exe comfy_mcp_server.py
```

**macOS or Linux**

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements.txt
export COMFY_ROOT="/path/to/ComfyUI"
export COMFY_URL="http://127.0.0.1:8188"
.venv/bin/python comfy_mcp_server.py
```

Set `COMFY_INPUT_DIR` and `COMFY_OUTPUT_DIR` before starting the server if the selected ComfyUI build uses separate folders. Keep the terminal open and verify the server below. [Configuration reference](docs/configuration.md)

### Verify the server

Leave both processes running and check `http://127.0.0.1:8189/health`. Both `mcp` and `comfyui` should report `ok`. On first run, Render Bridge creates a bearer token. Its default path is `%LOCALAPPDATA%\comfy-mcp\token.txt` on Windows or `${XDG_DATA_HOME:-$HOME/.local/share}/comfy-mcp/token.txt` on macOS and Linux. Keep the token in the server's private token file and your MCP client's user-scope config.

For custom ComfyUI folders, a different local port, or signed links for other devices, see the [configuration reference](docs/configuration.md).

## Connect an MCP client

Use an agent client that supports **Streamable HTTP** and a bearer token or custom HTTP header. Render Bridge and ComfyUI run together on the render host. The agent can run on that host or on another device with a network route to Render Bridge.

1. On **Connect** in the current Windows installer, start Render Bridge and select **Copy agent setup prompt**. After installation, use **Connect** in the Render Bridge panel to copy the prompt again. If the server is offline, select **Start server** first. Paste the prompt into the client on the render host.
2. For terminal setup, use the [copy-and-paste connection prompt](docs/connections.md#paste-a-connection-prompt). For a client on another device, set up its [private connection and credentials](docs/connections.md#connect-from-another-device) first.
3. Verify with `server_status` in the intended client session. If the client does not reload its MCP configuration, reopen it and run the check there.

For manual setup, follow the [client examples](docs/connections.md#configure-the-client) for Claude Code, Codex, VS Code Copilot, or another compatible client.

## Make your first render

After installing the [Z-Image Turbo starter files](docs/pipelines.md#recommended-first-pipeline), refresh the model list in ComfyUI and try this in your connected MCP client. The starter files are public. Sign in to a service only if the model you choose requires access or your workflow uses a paid provider. [Accounts and credentials](docs/pipelines.md#accounts-and-credentials)

> Call `server_status`, `list_jobs`, and `list_models` on Render Bridge. If ComfyUI is reachable, Gaming Mode is off, and the `z_image_turbo` preset is available, use `generate_image` to make one 512 × 512 PNG of a small watercolor lighthouse with transparency off. Give me its signed download link. If a prerequisite is missing, tell me which one.

Open the downloaded PNG to verify your first render. Then try video or 3D with an image suited to the pipeline.

- **Video.** Download **Wan Animate 2** in **Models**. Give your agent a character image and a short driving MP4 or MOV that shows the motion you want. Ask it to run `animate_from_clip` in default quality with `transparent=false` and return the signed `video.url`. Download and play the MP4. [Make your first video](docs/pipelines.md#make-your-first-video)
- **3D.** Configure a compatible image-to-3D workflow and its weights. Give `image_to_3d` an image of one centered object and import the returned GLB into a 3D viewer. [Image-to-3D setup](docs/pipelines.md#image-to-3d)

For longer jobs, submit through `start_job` and use `get_job` to retrieve the signed file links. If the agent is on another device, open the file using a link reachable from that device. [Remote connections and file transfer](docs/connections.md#connect-from-another-device)

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

Paste this into an agent with terminal access on the computer where ComfyUI runs. If Render Bridge is already installed, use the [connection prompt](docs/connections.md#paste-a-connection-prompt).

```text
Install Render Bridge from source on this computer and connect my MCP client.
Use https://github.com/BlackRune-LLC/render-mcp-bridge and read its README and AGENTS.md.
Reuse an existing checkout if appropriate. Otherwise clone the official repository.
Check for Python 3.11 or newer with venv support and find my running local ComfyUI build.
Use setup_render_bridge.py install and setup_render_bridge.py start with py -3 on Windows or python3 on macOS and Linux. Reuse the intended Render Bridge data directory and ComfyUI settings if already configured. Preserve my existing files and client settings.
Configure a user-scoped render_bridge entry with Streamable HTTP using the saved MCP address and private token file. Keep the token out of chat, logs, and command lines.
Call server_status to verify the MCP connection. If this session cannot reload its settings, tell me to open a fresh session and verify there. Tell me how to start and stop this installation.
Ask before installing system software, model weights, or nodes, and before submitting a render. Leave administrator prompts and account sign-ins to me.
```

## Support

For setup or runtime problems, include the Render Bridge version, operating system, ComfyUI build, and the step that failed. On Windows, **Settings** shows the version and can open your local crash logs. Review anything you attach and keep tokens, signed file links, and private paths out of the issue. [Open a support issue](https://github.com/BlackRune-LLC/render-mcp-bridge/issues/new)

Use the private reporting instructions for security issues. [Security reporting](SECURITY.md)

## License

Render Bridge's source and documentation are MIT licensed. Selected graph builders adapt settings from Comfy Org's MIT-licensed [workflow templates](https://github.com/Comfy-Org/workflow_templates). ComfyUI is a separate application distributed under [GPL-3.0](https://github.com/Comfy-Org/ComfyUI/blob/master/LICENSE). Models and custom nodes have their own upstream terms.
