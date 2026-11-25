# Use Render Bridge with an agent

Render Bridge exposes ComfyUI jobs and workflows over MCP. ComfyUI runs on the render host and Render Bridge connects to its configured API and asset folders. Follow the user's request for which host, client, and project to use.

## Set up and connect

- Follow the official installer and prerequisites, then choose the user's existing ComfyUI build or help them start one. Use the exact API port and folder paths for the selected build. [Setup guide](README.md#install-and-run)
- If the user provides a copied setup prompt, use its MCP URL and private token file path. Register a Streamable HTTP server in the intended client's user configuration. Preserve existing entries and ask for permission if that client requires approval for configuration edits. [Connection guide](docs/connections.md)
- Use the private endpoint and credential provisioned for the agent's device. Keep token contents in the client's private authentication settings or secret store. Exclude them from messages, command-line arguments, logs, and repository files.
- Verify the connection with `server_status` and `list_models`. Use `list_jobs` to check the queue. These calls let you verify setup without generating assets. If the current session cannot reload a client configuration change, ask the user to reopen it and retry.

## Run and retrieve work

- Use `server_status` and `list_models` to check availability, GPU capacity, and installed pipelines. If Gaming Mode is on or the server is unavailable, report that state. Choose a pipeline appropriate for the user's hardware and task. [Pipeline and model guide](docs/pipelines.md)
- For long renders, use `start_job` and follow the job with `get_job`. Use `list_jobs` to avoid submitting duplicates or interfering with other work. Ask before installing model weights or custom nodes.
- For a file on the agent's device, request `upload_link` and send it using the signed URL. Pass the returned filename and subfolder with `source="input"` to tools that take an image reference.
- Download results using the returned signed URL reachable from your device. Treat signed links as credentials until they expire. Preserve source files, existing assets, and settings when saving or copying results to the user's chosen folder. [File transfer guide](docs/connections.md#verify-access-and-transfer-files)
