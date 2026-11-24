# Installer releases

Build the wizard on the target platform with the pinned Cargo lockfile. Run the tests on that exact revision before packaging. The desktop wizard installs the bundled Python server, so verify it on a clean user profile with Python 3.11 or newer and a local ComfyUI API.

```sh
cargo test --locked --manifest-path installer/Cargo.toml -- --include-ignored
cargo build --release --locked --manifest-path installer/Cargo.toml
python -m pip install -r requirements.txt
python -B -m unittest discover
```

Run the Python commands inside an isolated virtual environment. Have the clean-machine check cover instance selection, fresh install, an interrupted upgrade, Start & Verify, the opt-in shortcut, and shutdown. A simulated ComfyUI backend is enough to exercise setup and the authenticated status call without loading weights.

## Windows x64

Use the Windows SDK's x64 SignTool and the release signing identity. Set `$SigningThumbprint` to its certificate thumbprint and `$TimestampUrl` to the signing provider's RFC 3161 endpoint. Copy the release executable to `RenderBridgeSetup.exe`, sign it, then verify its signature before calculating the checksum.

```powershell
signtool sign /sha1 $SigningThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 RenderBridgeSetup.exe
signtool verify /pa /v RenderBridgeSetup.exe
Get-FileHash -Algorithm SHA256 RenderBridgeSetup.exe
```

The published release should include the executable, its checksum, and third-party notices. Test the signed download on a clean Windows profile and confirm the expected publisher in Windows file properties. [Microsoft SignTool documentation](https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool)

## macOS and Linux

Build on Apple silicon macOS and test the packaged app from Finder and Terminal. Verify the optional `.command` shortcut and wizard-owned process shutdown. Use Developer ID signing and notarization for a public macOS download. [Apple distribution guidance](https://developer.apple.com/developer-id/)

Build the Linux x64 installer against the oldest distribution you plan to support. Test startup, the folder chooser, the application-menu shortcut, and shutdown in a normal desktop session. Record the distro, display session, and runtime-library requirements with the asset. Include a checksum and third-party notices.
