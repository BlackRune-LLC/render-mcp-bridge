# Render Bridge configuration

In the Windows control panel, open **Settings** to change appearance, sign-in behavior, or storage. In **Advanced configuration**, **Open configuration file** opens the active Render Bridge JSON settings. Stop the server before editing, keep a copy of the original, then start it again to verify the updated ComfyUI connection.

- `COMFY_ROOT` is the ComfyUI data directory.
- `COMFY_URL` is the loopback HTTP API address of the local ComfyUI build.
- `COMFY_INPUT_DIR` and `COMFY_OUTPUT_DIR` are optional absolute overrides. They must match the folders used by that ComfyUI instance.

To keep your runtime, shortcuts, and token when reinstalling, select the existing Render Bridge installation directory. Use **Storage** to choose where to copy generated assets or their backups. To change where ComfyUI writes the originals or searches for models, configure that ComfyUI build. [ComfyUI folder settings](https://docs.comfy.org/installation/desktop/usage/instance-management) · [Model storage](pipelines.md#store-models-on-another-drive)

## Terminal and server options

Set these environment variables on the render host before starting the app or terminal server. A managed instance uses the app's loopback listener and ComfyUI pairing.

| Option | Purpose |
| --- | --- |
| `COMFY_MCP_DATA_DIR` | Choose a private runtime and data location. The desktop installer can also choose one. |
| `MCP_PORT` | MCP endpoint port, default `8189`. Update the client or tunnel if you change it. |
| `MCP_TOKEN_FILE` | Private file containing the MCP bearer token. Keep it outside shared folders. |
| `PUBLIC_URLS` | Comma-separated named base URLs for signed uploads and downloads, such as `local=http://127.0.0.1:8189,private=https://render.example.net`. |
| `MCP_HOST`, `MCP_ALLOW_REMOTE` | Listener address and explicit non-loopback opt-in for terminal servers. A private SSH tunnel or HTTPS proxy can keep the listener on loopback. |
| `IDLE_UNLOAD_MIN` | Minutes before requesting model unload after confirmed inactivity. Defaults to 10. Set to 0 to disable. |
| `JOB_TIMEOUT_MIN`, `QUEUE_WAIT_MAX_MIN`, `COMFY_BUSY_WAIT_S` | Time to run a job, wait in the queue, and wait for a busy backend. Defaults are 15 minutes, 240 minutes, and 60 seconds. |
| `UPLOAD_MAX_MB`, `DOWNLOAD_LINK_DAYS` | Upload size limit and lifetime for signed downloads, default 2048 MiB and one day. |
| `PAUSE_FLAG`, `PROVENANCE_DIR` | Optional local paths for Gaming Mode state and asset provenance. |
| `COMFY_3D_WORKFLOW`, `GLTF_TRANSFORM_CLI` | Optional 3D workflow and GLB processing integrations. |

Use `server_status` after a restart to check the backend, queue, and idle timeout. Verify a remote connection on the device where the client runs. [Remote connections](connections.md#connect-from-another-device)
