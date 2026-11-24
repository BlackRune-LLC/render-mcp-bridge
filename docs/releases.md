# Installer releases

Build the wizard on the target platform with the pinned Cargo lockfile. Run the tests on that exact revision before packaging. Verify that the setup app opens on a clean user profile before Python, ComfyUI, or an agent client is installed. Follow the [first-time prerequisites](../README.md#first-time-prerequisites) to complete the smoke test.

```sh
cargo test --locked --manifest-path installer/Cargo.toml -- --include-ignored
python -m pip install -r requirements.txt
python -B -m unittest discover
```

Run the Python commands inside an isolated virtual environment. Have the clean-machine check cover missing prerequisite guidance, detection after installation, instance selection, fresh install, an interrupted upgrade, and Start & Verify. Open **ComfyUI Jobs** from the wizard and check the queue in the opt-in Gaming Mode shortcut. Verify that the panel distinguishes running work, queued work, an idle queue, and an unavailable queue. Confirm that **Keep Bridge running** cancels a stop request and that the wizard asks before stopping when work is active or the queue is unavailable. A simulated ComfyUI backend is enough to exercise these checks and the authenticated status call without loading weights.

## Windows x64

Run `./installer/build-windows.ps1` from PowerShell to build the release executable with the Visual C++ runtime linked statically. Find it at `installer/target/x86_64-pc-windows-msvc/release/render-bridge-setup.exe`. Open it on a clean Windows system before adding a separate Visual C++ redistributable for other software. Check that declining the administrator request leaves the manual Python link available. Then approve the WinGet install and verify the wizard can detect Python.

Use the Windows SDK's x64 SignTool and the release signing identity. Set `$SigningThumbprint` to its certificate thumbprint and `$TimestampUrl` to the signing provider's RFC 3161 endpoint. Copy the release executable to `RenderBridgeSetup.exe`, sign it, then verify its signature before calculating the checksum.

```powershell
signtool sign /sha1 $SigningThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 RenderBridgeSetup.exe
signtool verify /pa /v RenderBridgeSetup.exe
Get-FileHash -Algorithm SHA256 RenderBridgeSetup.exe
```

The published release should include the executable, its checksum, and third-party notices. Test the signed download on a clean Windows profile and confirm the expected publisher in Windows file properties. [Microsoft SignTool documentation](https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool)

## macOS and Linux

Run `cargo build --release --locked --manifest-path installer/Cargo.toml` on the target platform.

Build on Apple silicon macOS and test the packaged app from Finder and Terminal. Verify the optional `.command` shortcut and wizard-owned process shutdown. Use Developer ID signing and notarization for a public macOS download. [Apple distribution guidance](https://developer.apple.com/developer-id/)

Build the Linux x64 installer against the oldest distribution you plan to support. Test startup, the folder chooser, the application-menu shortcut, and shutdown in a normal desktop session. Record the distro, display session, and runtime-library requirements with the asset. Include a checksum and third-party notices.
