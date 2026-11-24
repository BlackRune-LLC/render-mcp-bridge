# Privacy and network use

This describes the current Render Bridge source and installers built from it. Render Bridge runs on a computer you control. It sends no telemetry, crash reports, usage reports, prompts, renders, or device identifiers to BlackRune LLC. It has no account system or automatic update checks. Rendering, queue checks, and model management connect directly to ComfyUI on the same computer. Render Bridge disables the MCP SDK's built-in tracing middleware.

## What stays on your computer

| Local data | What it is used for |
| --- | --- |
| Installation and preferences | The Render Bridge runtime, ComfyUI address and folders, appearance, asset destinations, and Gaming Mode state. Windows creates its app shortcut and uses a sign-in shortcut when you enable that option. |
| MCP token and clipboard | Render Bridge stores its bearer token in a private local file for authentication and signing file links. When you select **Copy agent setup prompt**, it puts the endpoint and token file path on the system clipboard, without including the token itself. Your MCP client may save the token separately in its private user configuration or secret store. |
| Job recovery | The `jobs/` journal keeps job status, timestamps, identifiers, and output file references. While running, the server prunes finished records older than 24 hours and limits the journal to 256 recent records. |
| Diagnostics | Server messages appear in the terminal or local logs used to run the server. The latest crash report per component stays in the default user data folder and contains sanitized diagnostic metadata. Render Bridge never uploads it. [Local crash logs](docs/configuration.md#local-crash-logs) |
| Project and asset history | The `provenance/` folder can contain original and effective prompts, workflow settings, seeds, input and output names, hashes, project context, copies of style reference images, and approval notes. These records remain until you remove them. |
| Images and other assets | ComfyUI stores inputs and outputs in its configured folders. Render Bridge copies outputs to the library or backup folder you select when you request it. A synced or network folder follows the rules of the storage service you chose. |

The default Render Bridge data folder is `%LOCALAPPDATA%\comfy-mcp` on Windows and `${XDG_DATA_HOME:-$HOME/.local/share}/comfy-mcp` on macOS and Linux. Setup shows the location if you choose a different folder. Make backups before removing data you want to keep. ComfyUI and your MCP client manage their own data separately. [Configuration reference](docs/configuration.md)

## Where network requests go

| Your action | Network activity |
| --- | --- |
| Install Render Bridge or an approved update | The installer uses pip to get Python packages from the configured index. The index receives ordinary connection information and pip's platform information, such as the OS, CPU architecture, and Python version. For offline setup, point pip at a local wheelhouse with `PIP_NO_INDEX=1` and `PIP_FIND_LINKS`. [pip install reference](https://pip.pypa.io/en/stable/cli/pip_install/) · [pip’s request metadata](https://github.com/pypa/pip/blob/main/src/pip/_internal/network/session.py#L75-L165) |
| Choose WinGet to install Python | WinGet contacts Microsoft's package source and the installer host. WinGet follows its own Windows diagnostic settings. The installer also provides a manual Python link. [WinGet telemetry settings](https://learn.microsoft.com/en-us/windows/package-manager/winget/settings#telemetry-settings) |
| Choose **Update** or **Install update** | GitHub serves release information or the installer. GitHub receives ordinary connection information such as your IP address and the fixed `RenderBridge` HTTP user-agent. Windows may contact certificate services while verifying a download. Render Bridge sends no job, token, or local configuration data in these requests. |
| Open a documentation, prerequisite, or model link | Your browser connects to the selected provider. The provider handles its own downloads and sign-in. Requesting access to a gated model can share your Hugging Face user information with its author. The wizard checks for installed model files locally. [Hugging Face gated models](https://huggingface.co/docs/hub/models-gated) |
| Connect an MCP client | Authorized clients can receive requested tool results, such as prompts, previews, file links, model filenames, and GPU details. Another device uses the connection you configure. A cloud-backed client may send data to its provider even when its Render Bridge connection is local. [Device connections](docs/connections.md#connect-from-another-device) |

ComfyUI and its nodes run separately. Comfy Desktop manages its own updates and optional telemetry. Partner Nodes call external services, and custom nodes can make their own network requests. The optional glTF Transform integration passes local files to the CLI you configure and leaves network reads disabled in the official CLI. [Comfy Desktop privacy policy](https://comfy.org/privacy/desktop/) · [ComfyUI Partner Nodes](https://docs.comfy.org/tutorials/partner-nodes/overview) · [glTF Transform CLI](https://github.com/donmccurdy/glTF-Transform/blob/main/packages/cli/src/cli.ts#L1713-L1718)

You can check a Windows installer against the SHA-256 published with its release and inspect its signature in Windows. A checksum confirms the file matches the published asset. An Authenticode signature identifies the signer when present. The public v0.1.0 download is unsigned and predates this review. [Official releases](https://github.com/BlackRune-LLC/render-mcp-bridge/releases) · [Verify a Windows file signature](https://learn.microsoft.com/en-us/windows/win32/seccrypto/using-signtool-to-verify-a-file-signature)
