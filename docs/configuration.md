# Render Bridge configuration

In the Windows control panel, open **Settings** to change appearance, sign-in behavior, or storage. In **Advanced configuration**, **Open configuration file** opens the active Render Bridge JSON settings. Stop the server before editing, keep a copy of the original, then start it again to verify the updated ComfyUI connection.

## Theme plugins

On Windows, open **Settings → Custom theme** to edit a color and select **Apply theme**. Edit Dark and Light palettes separately. **System** uses the palette that matches Windows appearance. **Export theme** saves a shareable JSON file and **Import theme** applies an existing file to this installation. **Reset to defaults** restores the built-in colors.

A theme plugin contains a name and color overrides. Each value is a six-digit hex color. Omitted colors use the built-in palette. Export an edited theme to use as a starting point.

```json
{
  "version": 1,
  "name": "Lilac",
  "dark": { "primary": "#9F7AFB", "destructive": "#A23243" },
  "light": { "accent": "#533585" }
}
```

Both palettes support `background`, `surface`, `rail`, `selection`, `rail_text`, `text`, `muted`, `border`, `accent`, `primary`, `success`, `warning`, `error`, `destructive`, and `logo`.

Render Bridge reads the file locally and saves the active palette as `appearance-palette.json` in its installation folder. The file changes colors only and contains no executable code.

To keep your runtime, shortcuts, and token when reinstalling, select the existing Render Bridge installation directory. Use **Storage** to choose where to copy generated assets or their backups. To change where ComfyUI writes the originals or searches for models, configure that ComfyUI build. [ComfyUI folder settings](https://docs.comfy.org/installation/desktop/usage/instance-management) · [Model storage](pipelines.md#store-models-on-another-drive)

## Windows installation, repair, and removal

On the wizard's **Install** step, choose a **Render Bridge installation folder** or accept the default. Select an empty folder for a new install. The **Settings → Storage** panel shows the folder for an installed copy.

To repair Render Bridge, exit the tray app and stop that installation's Render Bridge server. Run the standard setup wizard again, select the same ComfyUI build, and choose the same Render Bridge installation folder. Setup checks a replacement runtime before activating it.

To uninstall, run the setup wizard from a location outside the installation folder. On **Prerequisites**, choose **Uninstall Render Bridge**, select the installation folder, and confirm. Close the matching tray app and stop its Render Bridge server first. The uninstaller removes the runtime and tray files it can verify and the shortcuts that still point to that installation. It preserves saved settings, the private token, project records, job history, crash reports, and ComfyUI assets. If an older installation has no ownership information, repair it in place first. Older runtime files whose ownership cannot be verified stay in the selected folder.

## Local crash logs

In the Windows control panel, use **Settings → Open crash logs** after a report exists. You can also open the folder directly after a crash.

- **Windows** `%LOCALAPPDATA%\comfy-mcp\crash-logs`
- **macOS and Linux** `${XDG_DATA_HOME:-$HOME/.local/share}/comfy-mcp/crash-logs`

Render Bridge keeps the latest JSON report for each component in the default user data location, including when you choose another installation folder. On Windows, macOS, and Linux, the desktop app uses `desktop.json` for panics and window errors. The server and terminal installer use `server.json` and `terminal-installer.json` for unexpected Python errors at their entry points. The reports contain basic diagnostics and omit exception messages, full file paths, credentials, prompts, and memory dumps. Fatal operating system errors and forced termination can prevent a report from being written. Sending a report for support is up to you. [Privacy and network use](../PRIVACY.md)

If your operating system redirects or syncs your user profile, the reports follow that profile's storage settings.

## Terminal and server options

For desktop setup, choose the ComfyUI build and folders in the wizard. The terminal installer saves the same pairing. Set `COMFY_INPUT_DIR` or `COMFY_OUTPUT_DIR` before running `setup_render_bridge.py install` if the build uses separate folders. For a manual server launch, set the pairing variables before starting `comfy_mcp_server.py`. Render Bridge connects directly to ComfyUI on loopback and bypasses system HTTP proxies.

Set other environment variables on the render host before starting Render Bridge. For clients on another device, configure the MCP listener and signed file URLs for that connection. [Remote connections](connections.md#connect-from-another-device)

| Option | Purpose |
| --- | --- |
| `COMFY_ROOT` | The ComfyUI data directory, default `~/ComfyUI` in the terminal installer and manual setup. |
| `COMFY_INPUT_DIR`, `COMFY_OUTPUT_DIR` | Absolute folders used by the selected ComfyUI build. Default `input` and `output` under `COMFY_ROOT`. |
| `COMFY_URL` | Loopback HTTP or HTTPS API address for ComfyUI, default `http://127.0.0.1:8188`. Set this if your build uses a different local port. |
| `COMFY_MCP_DATA_DIR` | Choose a private runtime and data location. The desktop installer can also choose one. |
| `MCP_PORT` | MCP endpoint port, default `8189`. Setup remembers a custom port in the installation's `mcp-port` file so shortcuts and updates reuse it. To change a saved port, stop Render Bridge, edit or remove that file, and update your MCP client or tunnel. |
| `MCP_TOKEN_FILE` | Private file containing the MCP bearer token. Keep it outside shared folders. |
| `PUBLIC_URLS` | Comma-separated named base URLs for signed uploads and downloads. Defaults to loopback at the MCP port. Example `local=http://127.0.0.1:8189,private=https://render.example.net`. |
| `MCP_HOST`, `MCP_ALLOW_REMOTE` | MCP listen address, default `127.0.0.1`, and explicit opt-in for a terminal listener on another address. A private SSH tunnel or HTTPS proxy can keep the listener on loopback. |
| `IDLE_UNLOAD_MIN` | Minutes before requesting model unload after confirmed inactivity. Defaults to 10. Set to 0 to disable. |
| `JOB_TIMEOUT_MIN`, `QUEUE_WAIT_MAX_MIN`, `COMFY_BUSY_WAIT_S` | Time to run a job, wait in the queue, and wait for a busy backend. Defaults are 15 minutes, 240 minutes, and 60 seconds. |
| `UPLOAD_MAX_MB`, `DOWNLOAD_LINK_DAYS` | Upload size limit and lifetime for signed downloads, default 2048 MiB and one day. |
| `PAUSE_FLAG`, `PROVENANCE_DIR` | Optional local paths for Gaming Mode state and asset provenance. |
| `COMFY_3D_WORKFLOW`, `GLTF_TRANSFORM_CLI` | Optional 3D workflow and GLB processing integrations. |

Use `server_status` after a restart to check the backend, queue, and idle timeout. Verify a remote connection on the device where the client runs. [Remote connections](connections.md#connect-from-another-device)
