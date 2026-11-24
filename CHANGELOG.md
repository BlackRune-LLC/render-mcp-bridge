# Render Bridge changelog

## 0.2.0 (unreleased)

- Guided setup checks Python and finds the correct local ComfyUI instance. Choose an installation folder, an optional asset library and backup folder, and a separate model location.
- The Windows tray panel shows jobs and progress, has optional hidden sign-in startup, and provides server controls, Gaming Mode, and a resizable Settings window with theme choices.
- Windows setup starts the server through the installed panel and can close while the server keeps running. The Connect tab can verify a signed first PNG from the user's agent and rotate a stopped server's token.
- The Models page checks disk space, resumes interrupted downloads, verifies installed catalog files, and offers repairs that back up the originals.
- Setup stages a verified runtime before activating it. On Windows, the setup wizard can repair an installation in place or uninstall its registered app files and shortcuts while retaining user data.
- Windows builds display product, publisher, and version information. Windows updates check the official download's integrity and signature before installing.
- Render Bridge documents local data and explicit network requests, disables server tracing export, and keeps sanitized crash reports locally.

## 0.1.0

- First public release with the Render Bridge MCP server and an unsigned Windows x64 setup app. [GitHub release](https://github.com/BlackRune-LLC/render-mcp-bridge/releases/tag/v0.1.0)
