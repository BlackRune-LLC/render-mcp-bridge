# Render Bridge configuration

In the Windows control panel, open **Settings** to change appearance, sign-in behavior, or storage. In **Advanced configuration**, **Open configuration file** opens the active Render Bridge JSON settings. Stop the server before editing, keep a copy of the original, then start it again to verify the updated ComfyUI connection.

To keep your runtime, shortcuts, and token when reinstalling, select the existing Render Bridge installation directory. Use **Storage** to choose where to copy generated assets or their backups. To change where ComfyUI writes the originals or searches for models, configure that ComfyUI build. [ComfyUI folder settings](https://docs.comfy.org/installation/desktop/usage/instance-management) · [Model storage](pipelines.md#store-models-on-another-drive)

## Terminal and server options

For desktop setup, choose the ComfyUI build and folders in the wizard. The terminal installer saves the same pairing. Set `COMFY_INPUT_DIR` or `COMFY_OUTPUT_DIR` before running `setup_render_bridge.py install` if the build uses separate folders. For a manual server launch, set the pairing variables before starting `comfy_mcp_server.py`. Render Bridge connects directly to ComfyUI on loopback and bypasses system HTTP proxies.

Set other environment variables on the render host before starting Render Bridge. For clients on another device, configure the MCP listener and signed file URLs for that connection. [Remote connections](connections.md#connect-from-another-device)

| Option | Purpose |
| --- | --- |
| `COMFY_ROOT` | The ComfyUI data directory, default `~/ComfyUI` in the terminal installer and manual setup. |
| `COMFY_INPUT_DIR`, `COMFY_OUTPUT_DIR` | Absolute folders used by the selected ComfyUI build. Default `input` and `output` under `COMFY_ROOT`. |
| `COMFY_URL` | Loopback HTTP or HTTPS API address for ComfyUI, default `http://127.0.0.1:8188`. Set this if your build uses a different local port. |
| `COMFY_MCP_DATA_DIR` | Choose a private runtime and data location. The desktop installer can also choose one. |
| `MCP_PORT` | MCP endpoint port, default `8189`. Update the client or tunnel if you change it. |
| `MCP_TOKEN_FILE` | Private file containing the MCP bearer token. Keep it outside shared folders. |
| `PUBLIC_URLS` | Comma-separated named base URLs for signed uploads and downloads. Defaults to loopback at the MCP port. Example `local=http://127.0.0.1:8189,private=https://render.example.net`. |
| `MCP_HOST`, `MCP_ALLOW_REMOTE` | MCP listen address, default `127.0.0.1`, and explicit opt-in for a terminal listener on another address. A private SSH tunnel or HTTPS proxy can keep the listener on loopback. |
| `IDLE_UNLOAD_MIN` | Minutes before requesting model unload after confirmed inactivity. Defaults to 10. Set to 0 to disable. |
| `JOB_TIMEOUT_MIN`, `QUEUE_WAIT_MAX_MIN`, `COMFY_BUSY_WAIT_S` | Time to run a job, wait in the queue, and wait for a busy backend. Defaults are 15 minutes, 240 minutes, and 60 seconds. |
| `UPLOAD_MAX_MB`, `DOWNLOAD_LINK_DAYS` | Upload size limit and lifetime for signed downloads, default 2048 MiB and one day. |
| `PAUSE_FLAG`, `PROVENANCE_DIR` | Optional local paths for Gaming Mode state and asset provenance. |
| `COMFY_3D_WORKFLOW`, `GLTF_TRANSFORM_CLI` | Optional 3D workflow and GLB processing integrations. |

Use `server_status` after a restart to check the backend, queue, and idle timeout. Verify a remote connection on the device where the client runs. [Remote connections](connections.md#connect-from-another-device)
