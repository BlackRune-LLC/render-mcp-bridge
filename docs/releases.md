# Installer releases

Build the wizard on the target platform with the pinned Cargo lockfile. Run the tests on that exact revision before packaging. Verify that the setup app opens on a clean user profile before Python, ComfyUI, or an agent client is installed. Follow the [first-time prerequisites](../README.md#first-time-prerequisites) to complete the smoke test.

```sh
cargo test --locked --manifest-path installer/Cargo.toml -- --include-ignored
python -m pip install -r requirements.txt
python -B -m unittest discover
```

Run the Python commands inside an isolated virtual environment. Have the clean-machine check cover missing prerequisite guidance, detection after installation, instance selection, fresh install, an interrupted upgrade, and **Start & Verify**. Check the full upgrade path from v0.1.0 using its existing data directory and confirm that setup preserves the token. Open **ComfyUI Jobs** from the wizard and check the optional Gaming Mode shortcut separately. Confirm that **Keep Bridge running** cancels a stop request and that the wizard asks before stopping when work is active or the queue is unavailable. A simulated ComfyUI backend is enough to exercise these checks and the authenticated status call without loading weights.

## Windows x64

Run `./installer/build-windows.ps1` from PowerShell to build the release executable with the Visual C++ runtime linked statically. Find it at `installer/target/x86_64-pc-windows-msvc/release/render-bridge-setup.exe`. Open it on a clean Windows system before adding a separate Visual C++ redistributable for other software. Check that declining the administrator request leaves the manual Python link available. Then approve the WinGet install and verify the wizard can detect Python.

Check the compact panel on that Windows profile with the optional Gaming Mode shortcut left off.

1. Open the panel using **Open Render Bridge** on **Connect** and the per-user **Render Bridge** entry in Windows Start. Confirm both open the same panel for the selected local ComfyUI build.
2. In **Jobs**, check running, queued, idle, and unavailable queue states. Supply a progress event for a running job and verify that its stage and percentage agree with the event. Check the panel and dialogs at their default and minimum sizes in light and dark mode, and confirm that they follow changes to Windows appearance.
3. Use **Start server** to start a Render Bridge session from the panel. Once the panel confirms it is running, verify that the muted **Start server** button can still be clicked and keeps the existing session. Use the red **Stop server** button to review the queue, cancel once, and then confirm. Verify that the stop applies to the panel's session and that ComfyUI continues running. In a separate check, start Render Bridge in a terminal first and verify that the panel leaves it running.
4. Hide the panel to the Windows notification area and reopen it from the icon. Enable **Start hidden when I sign in**, sign out and back in, and confirm the panel starts in the notification area. Disable the setting and repeat sign-in to confirm the change.
5. Select **Update** with an eligible newer stable release available, with the installed version current, and with the release service unavailable. Verify that the panel offers **Install update**, confirms the current version, or shows a retryable error as appropriate. Check that the request starts on click and sends only the information needed to retrieve public release metadata. Local settings, paths, ComfyUI addresses, and tokens stay on the PC.
6. Confirm **Install update** on the test profile. Check that the panel downloads and verifies the official Windows installer before replacing the installed version. Confirm that the existing installation remains usable after an unsigned installer, an altered download, or an interrupted download. After a successful update, reopen from Start and verify the new version, saved settings, token, and sign-in preference.
7. Test the update with work in the queue and with the panel's Render Bridge session active. Verify that the panel protects running and queued jobs and handles its own session before replacing files. Confirm that independently launched Render Bridge and ComfyUI processes keep running.

Use the Windows SDK's x64 SignTool and the release signing identity. Set `$SigningThumbprint` to its certificate thumbprint and `$TimestampUrl` to the signing provider's RFC 3161 endpoint. Copy the release executable to `RenderBridgeSetup.exe`, sign it, then verify its signature before calculating the checksum.

```powershell
signtool sign /sha1 $SigningThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 RenderBridgeSetup.exe
signtool verify /pa /v RenderBridgeSetup.exe
Get-FileHash -Algorithm SHA256 RenderBridgeSetup.exe
```

The published release should include the executable, its checksum, and third-party notices. Test the signed download on a clean Windows profile and confirm the expected publisher in Windows file properties. Keep the current Windows download and README link active while signing or clean-profile verification is pending. Once the signed release is published, update the README to link to its versioned asset. [Microsoft SignTool documentation](https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool)

## macOS and Linux

Run `cargo build --release --locked --manifest-path installer/Cargo.toml` on the target platform.

Build on Apple silicon macOS and test the packaged app from Finder and Terminal. Verify the optional `.command` shortcut and wizard-owned process shutdown. Use Developer ID signing and notarization for a public macOS download. [Apple distribution guidance](https://developer.apple.com/developer-id/)

Build the Linux x64 installer against the oldest distribution you plan to support. Test startup, the folder chooser, the application-menu shortcut, and shutdown in a normal desktop session. Record the distro, display session, and runtime-library requirements with the asset. Include a checksum and third-party notices.
