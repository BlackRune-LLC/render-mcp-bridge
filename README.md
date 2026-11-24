# Black Rune Render Bridge

Connect an MCP client to your own ComfyUI installation. Render Bridge exposes the queue, background jobs, image generation and editing, and project asset records over authenticated Streamable HTTP. Tool availability depends on the models and nodes you install in ComfyUI. Render Bridge is an independent ComfyUI integration by BlackRune LLC.

## Install and run

1. Install and start [ComfyUI](https://docs.comfy.org/installation/manual_install) on the same host. Confirm its API responds at `http://127.0.0.1:8188/system_stats`. Find the ComfyUI directory that contains its `input` and `output` folders. For a Windows portable install, this is usually the inner `ComfyUI` directory.
2. Install Python 3.11 or later. Clone this repository or download its source, then open a terminal in that directory.
3. Set `COMFY_ROOT` to your ComfyUI directory and start the MCP server in a virtual environment.

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

Leave both processes running and check `http://127.0.0.1:8189/health`. Both `mcp` and `comfyui` should report `ok`. On first run, Render Bridge creates a bearer token. Its default path is `%LOCALAPPDATA%\comfy-mcp\token.txt` on Windows or `${XDG_DATA_HOME:-$HOME/.local/share}/comfy-mcp/token.txt` on macOS and Linux. Keep the token in the server's private token file and your MCP client's user-scope config.

| Setting | Default and purpose |
| --- | --- |
| `COMFY_ROOT` | `~/ComfyUI`. Set this to the actual ComfyUI data directory used for local file transfers. |
| `COMFY_URL` | `http://127.0.0.1:8188`. Set this if your ComfyUI API uses a different port. |
| `MCP_HOST`, `MCP_PORT` | `127.0.0.1` and `8189`. |
| `COMFY_MCP_DATA_DIR` | Per-user data directory. Stores the token, gaming-mode state, and project records. |
| `PUBLIC_URLS` | Served URLs. Defaults to a loopback URL at the MCP port. For a remote client, use a named URL such as `private=https://mcp.example.com`. Signed uploads and downloads use this address. |

For remote access, keep the MCP server bound to loopback and put an HTTPS reverse proxy or private tunnel in front of it. Forward `/mcp`, `/files/`, and `/upload/`. For a reverse proxy, send the upstream `Host` header as `127.0.0.1:8189` (or the configured loopback port), stream MCP responses, and omit query strings from access logs. Set `PUBLIC_URLS` to the client-facing URL. Binding the MCP process itself to a non-loopback address requires the explicit `MCP_ALLOW_REMOTE=1` setting. Give the bearer token only to clients you trust with access to ComfyUI and your asset records.

## Connect an MCP client

Add a user-scope server named `comfyui` with Streamable HTTP URL `http://127.0.0.1:8189/mcp` and header `Authorization: Bearer <token>`. Replace `<token>` in your private client settings with the contents of your token file. Preserve any MCP entries you already use. Use another name if `comfyui` is taken.

For Claude Code, merge this entry into the top-level `mcpServers` object in your user config.

```json
{
  "comfyui": {
    "type": "http",
    "url": "http://127.0.0.1:8189/mcp",
    "headers": { "Authorization": "Bearer <token>" }
  }
}
```

For Codex, add these tables to your user `config.toml`.

```toml
[mcp_servers.comfyui]
url = "http://127.0.0.1:8189/mcp"
tool_timeout_sec = 600.0

[mcp_servers.comfyui.http_headers]
Authorization = "Bearer <token>"
```

Start a fresh client session and call `server_status` and `list_jobs`. Then call `list_models` and check that the preset you want is available before you send a generation job. Install models and custom nodes in ComfyUI under their respective upstream licenses. Use `start_job` and `get_job` for long-running work.

Once you have an available preset, ask your agent to generate a test image with that preset and return its signed download link.

For optional image-to-3D tools, set `COMFY_3D_WORKFLOW` to your own compatible ComfyUI API-format workflow. It needs the official Pixal3D and TRELLIS.2 template layout, including `Save3DAdvanced`. For GLB optimization, install Node.js and the glTF Transform CLI, add `node` to your PATH, and set `GLTF_TRANSFORM_CLI` to the installed CLI's JavaScript entry point.

## AI server gaming mode

From a second terminal, use the same virtual environment and settings as the running server, including `COMFY_URL` and `COMFY_MCP_DATA_DIR` if set. On Windows, replace `python` below with `.\.venv\Scripts\python.exe`. On macOS or Linux, use `.venv/bin/python`.

```sh
python comfy_mcp_server.py gaming-mode on
python comfy_mcp_server.py gaming-mode status
python comfy_mcp_server.py gaming-mode off
```

Turning gaming mode on pauses new GPU work through the MCP server. If ComfyUI's queue is idle, the command asks it to unload models and free memory. If it is busy, let the existing work finish and run `gaming-mode on` again to request the unload. The pause persists across restarts. Check `server_status` from your MCP client to confirm it, then switch gaming mode off when you're ready to generate again. Keep ComfyUI running separately.

## Ask your agent to set it up

Give the following prompt to an agent with terminal access on the machine where you want to run the server.

> Set up Black Rune Render Bridge on this machine using the README from https://github.com/BlackRune-LLC/render-mcp-bridge. Find or help me install my own ComfyUI using its official instructions, verify its API, and identify its input/output directory. Install the MCP server's Python requirements in a virtual environment. Use loopback defaults and keep the token and runtime records in the private user data directory. Register it in the user-scope MCP config for the client I actually use, keeping all existing entries. Handle the bearer token locally and never print it, ask for it in chat, pass it as a command argument, or save it to a project file. If you cannot use it securely, ask me to add it directly to my private client settings. Verify the health endpoint and call `server_status` and `list_jobs` in a fresh MCP client session. Show me the start command and gaming-mode controls. Ask before downloading any models or running a generation job.

## License

Render Bridge's source and documentation are MIT licensed. Selected graph builders adapt settings from Comfy Org's MIT-licensed [workflow templates](https://github.com/Comfy-Org/workflow_templates). ComfyUI is a separate application distributed under [GPL-3.0](https://github.com/Comfy-Org/ComfyUI/blob/master/LICENSE). Models and custom nodes have their own upstream terms.
