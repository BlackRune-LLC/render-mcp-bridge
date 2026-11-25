# Render Bridge changelog

## 0.2.0

- Guided setup checks Python and finds the correct local ComfyUI instance. Choose an installation folder, an optional asset library and backup folder, and a separate model location.
- The Windows tray panel shows jobs and progress, has optional hidden sign-in startup, and provides server controls, Gaming Mode, and a resizable Settings window with theme choices.
- Windows setup starts the server through the installed panel and can close while the server keeps running. The Connect tab can verify a signed first PNG from the user's agent and rotate a stopped server's token.
- The Models page lists installed files, curated packs, licenses, and download destinations. It checks disk space, resumes transfers, and verifies or repairs installed files.
- Pipelines adds a visual graph, model variant switching, and a library of named workflows. Import or duplicate a pipeline, assign installed models to compatible loader slots, and save edits for future runs. Nodes shows the active ComfyUI inventory and lets you review compatible local node packs before installing them.
- The compact Jobs panel displays current-node progress. Settings supports reusable color palettes, and setup can reuse a configuration file while preserving the installation's saved choices.
- Setup stages a verified runtime before activating it. On Windows, the setup wizard can repair an installation in place or uninstall its registered app files and shortcuts while retaining user data.
- Windows builds display product and version information. **Update** checks GitHub and opens the official release download for manual installation. Automatic installation requires a valid Windows signature.
- Render Bridge documents local data and explicit network requests, disables server tracing export, and keeps sanitized crash reports locally.

## 0.1.0

- First public release with the Render Bridge MCP server and an unsigned Windows x64 setup app. [GitHub release](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/tag/v0.1.0)
