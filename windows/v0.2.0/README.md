# Render Bridge 0.2.0 for Windows x64

Use `RenderBridgeSetup.exe` from this directory. These are the installer bytes planned for the public release. The installer is unsigned and built from source commit `3b5c100296e13ee9707b2645c5b0cedbdeb31b89`.

Download [RenderBridgeSetup.exe](./RenderBridgeSetup.exe?raw=1) and [SHA256SUMS.txt](./SHA256SUMS.txt?raw=1) to the same folder. Open PowerShell in that folder and run the following before opening Setup.

```powershell
$expected = ((Get-Content -LiteralPath .\SHA256SUMS.txt -TotalCount 1) -split '\s+')[0]
$actual = (Get-FileHash -Algorithm SHA256 -LiteralPath .\RenderBridgeSetup.exe).Hash
if ($expected -ne 'a1fe456092ec052744e0f8b471cf73aaf808118168f0a9b90075e59a0e2c2f27' -or $actual -ne $expected) {
    throw 'RenderBridgeSetup.exe checksum mismatch.'
}
```

## Fresh install

- On a clean Windows x64 profile, run Setup and follow its Python and ComfyUI instructions. Select the intended local ComfyUI instance and installation folder.
- Select **Start & Verify**, connect an agent, and use **First render** to have the agent generate a PNG. Open the agent's download link and paste it into **Verify test PNG** in Setup. Select **Finish and keep running**, then reopen Render Bridge from the tray.

## Pipeline edits

- Export a ComfyUI workflow in **API Format**. Open **Pipelines → Add pipeline**, import the JSON, and name it. Check that the pipeline opens from **Your pipelines** after reopening the panel.
- If another compatible model is installed, select it in **Models → Installed → Use in pipeline**, choose the saved pipeline and loader slot, then select **Apply model** and **Save changes**. Reopen the pipeline and confirm the loader and **Copy agent prompt** carry the selected filename.

- In **Built-in pipelines**, open several step menus and try the **Motion model** dropdown. In **Nodes**, select a custom node and check its source, version, and review state. Check both the Dark and Light theme at the normal workspace size.

## Upgrade from v0.1.0

- On a separate profile with v0.1.0 installed, let jobs finish, stop Render Bridge, and exit the tray app. Run the same EXE and select the existing installation folder and ComfyUI instance.
- Check that the saved settings and token are preserved and the existing agent can reconnect. On the disposable profile, verify that uninstall removes the app and shortcuts while keeping settings and assets.

Report the installer SHA-256, Windows version, ComfyUI build, and the step that passed or failed. Share a redacted error or screenshot if helpful. Keep tokens, signed URLs, private paths, and raw logs out of the report.
