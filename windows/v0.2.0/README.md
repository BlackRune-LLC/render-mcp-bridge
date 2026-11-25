# Render Bridge 0.2.0 for Windows x64

Use `RenderBridgeSetup.exe` from this directory. These are the installer bytes planned for the public release. The installer is unsigned and built from source commit `35778fa084058a347c38496fa71ca691137ec0a9`.

Download [RenderBridgeSetup.exe](./RenderBridgeSetup.exe?raw=1) and [SHA256SUMS.txt](./SHA256SUMS.txt?raw=1) to the same folder. Open PowerShell in that folder and run the following before opening Setup.

```powershell
$expected = ((Get-Content -LiteralPath .\SHA256SUMS.txt -TotalCount 1) -split '\s+')[0]
$actual = (Get-FileHash -Algorithm SHA256 -LiteralPath .\RenderBridgeSetup.exe).Hash
if ($expected -ne '8e3b63792f2e2cd033e0db7211a11bbf755e953ac723767f8e9f90a7a6e2c22e' -or $actual -ne $expected) {
    throw 'RenderBridgeSetup.exe checksum mismatch.'
}
```

## Fresh install

- On a clean Windows x64 profile, run Setup and follow its Python and ComfyUI instructions. Select the intended local ComfyUI instance and installation folder.
- Select **Start & Verify**, connect an agent, and choose **Finish and keep running**. Reopen Render Bridge from the tray and have the agent make a first render. Check that the signed PNG opens.

## Upgrade from v0.1.0

- On a separate profile with v0.1.0 installed, let jobs finish, stop Render Bridge, and exit the tray app. Run the same EXE and select the existing installation folder and ComfyUI instance.
- Check that the saved settings and token are preserved and the existing agent can reconnect. On the disposable profile, verify that uninstall removes the app and shortcuts while keeping settings and assets.

Report the installer SHA-256, Windows version, ComfyUI build, and the step that passed or failed. Share a redacted error or screenshot if helpful. Keep tokens, signed URLs, private paths, and raw logs out of the report.
