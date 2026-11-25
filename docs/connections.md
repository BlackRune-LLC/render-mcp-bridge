# Connect agents to Render Bridge

Run Render Bridge and ComfyUI on the render host. Run your MCP client on that machine or on another device with a route to Render Bridge. The client needs Streamable HTTP and either a bearer-token field or custom HTTP request headers. Choose **HTTP** or **Streamable HTTP** in clients that offer several transport types. [MCP transport specification](https://modelcontextprotocol.io/specification/latest/basic/transports)

## Let your agent connect

### During installation

1. In the Windows installer, open **Connect**. If setup manages the server, select **Start & Verify**. Select **Copy agent setup prompt**.
2. Paste the prompt into the client you want to configure. For a server managed by setup, the prompt includes the exact local MCP URL and the path to the private token file. The agent can use the file through its private client settings. If you attached to an existing listener, replace the placeholders with the MCP URL and private credential reference that the client can use.
3. Approve any requested edit to your user-scope client config. If the client needs to reload its configuration, reopen it and ask it to call `server_status`.
4. For a managed installation, use **First render** for an optional PNG test. **Finish and keep running** leaves the server active in the installed Windows panel. For an attached server, select **Close setup**.

### Connect after installation

1. Open **Render Bridge** from Windows Start or its notification-area icon. For a managed server that is offline, select **Start server**.
2. Select **Connect** to open **Connect an agent**. Use **Copy agent setup prompt** under the default **Any agent** choice to connect a client on the render host. A managed server's prompt supplies its local MCP URL and private token file path. For manual setup, use **Copy address** and the private token file path from the prompt.
3. For a client on another device, choose **Another device** and select **Copy agent setup prompt**. Replace the placeholders with the URL and private credential reference that the agent can use. Use the same approach when the panel is attached to an existing listener. [Connect from another device](#connect-from-another-device)
4. Verify the connection with `server_status` in the client. Open a fresh session if it needs to reload its MCP configuration. For a managed server, open **Connect → First render test** for a PNG check after starting the server.

To return to **Connect** in the setup wizard, reopen the wizard and choose **Open existing setup** on **Prerequisites**. For a custom installation folder, choose **Browse for an installation** and select that folder.

### Rotate the connection token

1. Stop Render Bridge for the installation you want to rotate. Use **Stop server** if the panel started it, or stop the server in the terminal that launched it.
2. Stop every Render Bridge instance that uses this token file. In the installed panel, open **Connect**, choose **Rotate token**, then **Confirm rotation**. The replacement goes into the installation's private token file.
3. Start Render Bridge again. Update each client's private token or secret from that file, including the secret provisioned on any other device. Reopen clients that need to reload their configuration and call `server_status` to verify access.

After restart, the old token and previously signed upload and download URLs are invalid. Request new file URLs when needed. For a terminal installation, stop Render Bridge and run `setup_render_bridge.py rotate-token --confirm` with your usual Python launcher and the server's data directory, listen address, port, and token file settings.

### Paste a connection prompt

For terminal setup, start Render Bridge, replace the two bracketed values below, and paste the prompt into the intended client. The default MCP URL on the render host is `http://127.0.0.1:8189/mcp`. For a client on another device, establish its [secure connection](#connect-from-another-device) and provision the token privately on that device first. Use that device's reachable URL and its own token file path or secret name.

```text
Connect this MCP client to Render Bridge at [MCP URL] using Streamable HTTP. On this device, the bearer token is available from [private token file path or configured secret name].

Use the token through this client's private authentication settings. Add or update a user-scoped server and preserve my other settings. Keep the name of an existing entry for this endpoint. For a new entry, choose a name allowed by my client, such as render-bridge. Ask me first if you need permission to edit my settings. Keep the token out of chat, logs, and command-line arguments.

Verify the connection with the read-only server_status tool. If this session can't reload its MCP configuration, tell me to restart the client or open a fresh session and run the same check.
```

## Configure the client

1. Start Render Bridge so it creates its private token file. Setup displays its path. The default is `%LOCALAPPDATA%\comfy-mcp\token.txt` on Windows or `${XDG_DATA_HOME:-$HOME/.local/share}/comfy-mcp/token.txt` on macOS and Linux.
2. Add a user-scoped server with a unique name your client accepts, such as `render-bridge`. If an entry already points to this endpoint, keep its name. Use the Render Bridge URL ending in `/mcp`. The default on the render host is `http://127.0.0.1:8189/mcp`. For a client on another device, use the URL from [Connect from another device](#connect-from-another-device).
3. If the client asks for a bearer token, supply the token value. If it asks for a header, use `Authorization` with value `Bearer <token>`. Give that credential to trusted agents with access to the server's tools and asset records. Keep it in the client's secret storage, environment, or private user settings.

The environment-based examples below read `RENDER_BRIDGE_TOKEN` from the client's environment. Use the client's secret manager if it provides environment variables. To use a private token file on the client device, substitute its path in one of the commands below and launch the client from the same shell.

```powershell
$env:RENDER_BRIDGE_TOKEN = (Get-Content -LiteralPath 'C:\path\to\private-token.txt' -Raw).Trim()
```

```sh
export RENDER_BRIDGE_TOKEN="$(cat /path/to/private-token.txt)"
```

An IDE, remote workspace, or hosted agent may run its MCP client in a separate environment. Provide the secret and network route there.

### Claude Code

Merge the `render_bridge` entry into the existing `mcpServers` object in your user configuration, typically `~/.claude.json` or `%USERPROFILE%\.claude.json`. For a custom profile, edit the corresponding user config. Open Claude Code and check the connection with `/mcp`. [Claude Code MCP configuration](https://code.claude.com/docs/en/mcp#user-scope)

```json
{
  "mcpServers": {
    "render_bridge": {
      "type": "http",
      "url": "http://127.0.0.1:8189/mcp",
      "headers": { "Authorization": "Bearer ${RENDER_BRIDGE_TOKEN}" }
    }
  }
}
```

### Codex

Add the table to your user `config.toml`, typically under `~/.codex` or `%USERPROFILE%\.codex`. If you set `CODEX_HOME`, use that directory. Check the entry with `codex mcp list` and verify it in a fresh session. [Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)

```toml
[mcp_servers.render_bridge]
url = "http://127.0.0.1:8189/mcp"
bearer_token_env_var = "RENDER_BRIDGE_TOKEN"
tool_timeout_sec = 600.0
```

### VS Code Copilot

For the Copilot harness, merge the entry into the Copilot user MCP configuration, typically `~/.copilot/mcp-config.json` or `%USERPROFILE%\.copilot\mcp-config.json`. Use `COPILOT_HOME` when configured. Select the server's tools in the intended session. [VS Code MCP configuration reference](https://code.visualstudio.com/docs/agents/reference/mcp-configuration)

```json
{
  "mcpServers": {
    "render_bridge": {
      "type": "http",
      "url": "http://127.0.0.1:8189/mcp",
      "headers": { "Authorization": "Bearer ${RENDER_BRIDGE_TOKEN}" },
      "tools": ["*"]
    }
  }
}
```

### OpenCode, Gemini CLI, and Antigravity

Use the same MCP address and bearer value with the fields below. Merge the entry into your private user configuration and keep your existing settings.

| Client | User configuration | Streamable HTTP and bearer fields |
| --- | --- | --- |
| OpenCode | Add your server under `mcp.servers` in `~/.config/opencode/opencode.json` or `opencode.jsonc`. OpenCode V1 used `mcp` directly. | Set `type` to `"remote"`, set `url` to your MCP address, set `oauth` to `false`, and set `headers.Authorization` to `"Bearer {env:RENDER_BRIDGE_TOKEN}"`. Check with `opencode mcp list`. [OpenCode MCP configuration](https://opencode.ai/docs/mcp-servers/#remote) · [OpenCode V1 configuration](https://opencode.ai/docs/migrate-v1/#mcp-servers) |
| Gemini CLI | Add `render-bridge` under `mcpServers` in `~/.gemini/settings.json`. Use a name without an underscore so Gemini's policy engine can identify the server. | Set `httpUrl` to your MCP address and `headers.Authorization` to `"Bearer ${RENDER_BRIDGE_TOKEN}"`. Check with `gemini mcp list`. [Gemini CLI HTTP headers](https://geminicli.com/docs/tools/mcp-server/#http-based-mcp-server-with-custom-headers) · [Gemini CLI settings](https://geminicli.com/docs/reference/configuration/) |
| Google Antigravity | Add your server under `mcpServers` in your private `mcp_config.json`. In the App, open **Customizations → Installed → MCP Servers** and use **Copy Config File Path**. The standard CLI and IDE user config is `~/.gemini/config/mcp_config.json`. | Set `serverUrl` to your MCP address and supply `headers.Authorization` as `"Bearer <token>"`, replacing the placeholder privately. [Google Antigravity MCP configuration](https://antigravity.google/docs/mcp#custom-headers) |

Open a fresh session if the client needs to reload its settings, then call `server_status`. If a client requires HTTPS, use the private HTTPS setup below.

## Connect from another device

Choose a LAN or private VPN route to Render Bridge from the environment running the client. Keep ComfyUI's backend API, usually on port 8188, on the render host.

### SSH tunnel for one device

If the render host already runs an SSH server reachable on your LAN or private VPN, leave Render Bridge bound to `127.0.0.1:8189`. On the client device, replace `user@render-host` with your SSH destination and keep the command running. Use the same available port at both ends so the default signed file URLs also work on the client device. [OpenSSH port forwarding](https://man.openbsd.org/ssh.1#L)

```sh
ssh -N -o ExitOnForwardFailure=yes -L 127.0.0.1:8189:127.0.0.1:8189 user@render-host
```

Configure a client that accepts loopback HTTP with `http://127.0.0.1:8189/mcp`. The SSH tunnel encrypts the connection between the two devices.

### HTTPS on a private network

1. Give the reverse proxy a hostname reachable by the client and a TLS certificate the client trusts, such as `render.example.com`. Restrict reachability to your LAN or private network using the proxy host's firewall or that network's access controls.
2. Route `/mcp`, `/files/`, and `/upload/` to Render Bridge at `http://127.0.0.1:8189` on the render host. Set the upstream `Host` to `127.0.0.1:8189`, or your configured loopback host and port. Preserve the HTTP method, authentication and MCP headers, paths, and query strings. Stream MCP responses without proxy buffering, allow the intended upload size, and omit signed query strings and authorization headers from proxy logs. [NGINX proxy directives](https://nginx.org/en/docs/http/ngx_http_proxy_module.html)
3. Set `PUBLIC_URLS` to the client-facing **base URL** in the environment that starts Render Bridge. To run this setup from a terminal, stop any Render Bridge session on that port and run the launcher from the source directory with the same user data directory you set up. Replace the example hostname with yours.

**Windows PowerShell**

```powershell
$env:MCP_HOST = '127.0.0.1'
$env:MCP_PORT = '8189'
$env:PUBLIC_URLS = 'private=https://render.example.com,local=http://127.0.0.1:8189'
py -3 .\setup_render_bridge.py start
```

**macOS or Linux**

```sh
MCP_HOST=127.0.0.1 MCP_PORT=8189 PUBLIC_URLS='private=https://render.example.com,local=http://127.0.0.1:8189' python3 setup_render_bridge.py start
```

Add `https://render.example.com/mcp` to the client. Keep the terminal launcher running while the client uses it. The same access requirements apply to a hosted agent. Use an approved connector or a runtime on the private network if the service cannot reach your private endpoint directly.

The server supports explicit direct binding using `MCP_ALLOW_REMOTE=1`, a specific listen address in `MCP_HOST`, and a client-visible `PUBLIC_URLS`. Keep that listener inside an encrypted, access-controlled network. Use the loopback-and-HTTPS proxy setup for TLS at the client-facing endpoint.

## Verify access and transfer files

Open a fresh session in the intended client and call `server_status`, `list_jobs`, then `list_models`. Use the client's server or tools panel to confirm the tools are available. Verify a connection with actual tool calls if you use a secret store or environment-variable authentication. The installer's saved-configuration check can recognize the address without resolving the client's secret.

- **File downloads.** Use the `url` returned by the tool, or choose a reachable entry in `urls`. Render Bridge uses the first `PUBLIC_URLS` entry for the default link. Signed links include their own credential and expire.
- **File uploads.** Call `upload_link` with the local file's name. Send the file with an HTTP `PUT` that includes its length, such as the `curl -T` command returned by the tool. Use the returned filename and subfolder with `source="input"` for tools that take a file reference. Each upload link works once. Use the entire signed URL, including its query string, and keep it private.
- **Endpoint or authentication error.** From the render host, check `http://127.0.0.1:8189/health`. It reports process and ComfyUI status. If the client gets HTTP 401 from `/mcp`, check the bearer value, secret environment, and proxy header forwarding. If the client times out, check the tunnel, private route, and firewall from the client's environment.
- **Tools work but file links fail.** Check that the link uses an address reachable by the device downloading or uploading the file. Set `PUBLIC_URLS` on the terminal launcher and restart it if the advertised address is wrong. Check expiration, query-string forwarding, and upload limits at the proxy.
- **ComfyUI is stopped or paused.** Start the configured ComfyUI instance on the render host or resume new work through Render Bridge's Gaming Mode. Check `COMFY_URL` if the Bridge is listening but the backend fails.
