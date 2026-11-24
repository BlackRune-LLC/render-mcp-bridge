# Installer releases

Build the wizard on the target platform with the pinned Cargo lockfile. Run the tests on that exact revision before packaging. Verify that the setup app opens on a clean user profile before Python, ComfyUI, or an agent client is installed. Follow the [first-time prerequisites](../README.md#first-time-prerequisites) to complete the smoke test.

Set the release version in `installer/Cargo.toml` and refresh the lockfile and [changelog](../CHANGELOG.md). Update the version fields in `installer/assets/render-bridge.rc` and regenerate `render-bridge.res` from that directory with the Windows SDK command `rc /nologo /fo render-bridge.res render-bridge.rc`. The Windows build checks the executable version and product fields. [Windows version resources](https://learn.microsoft.com/en-us/windows/win32/menurc/versioninfo-resource)

Inspect the source archive for the exact release commit. The public root `AGENTS.md` guides users' agents. Check that the archive excludes internal agent instructions, `.agents`, `.claude`, `.codex`, local settings, and credentials.

```sh
cargo test --locked --manifest-path installer/Cargo.toml -- --include-ignored
python -m pip install -r requirements.txt
python -B -m unittest discover
```

Run the Python commands inside an isolated virtual environment. Have the clean-machine check cover missing prerequisite guidance, detection after installation, instance selection, fresh install, an interrupted upgrade, and **Start & Verify**. Check the full upgrade path from v0.1.0 using its existing data directory and confirm that setup preserves the token. Open **ComfyUI Jobs** from the wizard and check the optional Gaming Mode shortcut separately. Confirm that **Keep Bridge running** cancels a stop request and that the wizard asks before stopping when work is active or the queue is unavailable. Use a simulated ComfyUI backend for the connection and installer checks.

Verify generation separately on a clean render host with a supported GPU and an actual MCP client. Start without weights, confirm the connection succeeds, and verify that the selected starter reports its missing weights. Install the approved weights and nodes, then generate and download one 512 × 512 PNG with `generate_image`, one playable MP4 with `animate_loop`, and one importable GLB with `image_to_3d`. Open each file from its signed link on the client device. Record the installer version, ComfyUI version, GPU, model choices, and results. [First-render walkthrough](../README.md#make-your-first-render)

After installing dependencies, disconnect internet access and verify local startup, MCP authentication, queue status, and a file transfer against a simulated ComfyUI backend. Repeat the terminal check with an HTTP proxy configured and confirm the local backend still receives the requests directly. From a clean profile, check that opening the app and leaving it idle creates no internet requests from Render Bridge. Check intentional downloads and Update separately against the [privacy disclosure](../PRIVACY.md).

## Windows x64

Run `./installer/build-windows.ps1` from PowerShell to build the release executable with the Visual C++ runtime linked statically. Find it at `installer/target/x86_64-pc-windows-msvc/release/render-bridge-setup.exe`. Open it on a clean Windows system before adding a separate Visual C++ redistributable for other software. Check that declining the administrator request leaves the manual Python link available. Then approve the WinGet install and verify the wizard can detect Python.

Check the compact panel on that Windows profile with the optional Gaming Mode shortcut left off.

1. Open the panel using **Open Render Bridge** on **Connect** and the per-user **Render Bridge** entry in Windows Start. Confirm both open the same panel for the selected local ComfyUI build.
2. In **Jobs**, check running, queued, idle, and unavailable queue states. Supply a progress event for a running job and verify that its stage and percentage agree with the event. On first launch verify the Dark default. Open **Settings** and check that the separate window fits the display and that scrolling reaches Storage and Advanced configuration at high scaling. Verify that **Done** is always reachable and closing Settings leaves Jobs open. Choose **Light**, then reopen the panel to verify the preference is saved. Check the layout and both Gaming Mode states in each theme. Choose **System** and confirm that the panel follows Windows appearance.
3. Use **Start server** to start a Render Bridge session from the panel. Once the panel confirms it is running, verify that the muted **Start server** button can still be clicked and keeps the existing session. Use the red **Stop server** button to review the queue, cancel once, and then confirm. Verify that the stop applies to the panel's session and that ComfyUI continues running. In a separate check, start Render Bridge in a terminal first and verify that the panel leaves it running.
4. Hide the panel to the Windows notification area and reopen it from the icon. Enable **Start hidden when I sign in**, sign out and back in, and confirm the panel starts in the notification area. Disable the setting and repeat sign-in to confirm the change.
5. Select **Update** with an eligible newer stable release available, with the installed version current, and with the release service unavailable. Verify that the panel offers **Install update**, confirms the current version, or shows a retryable error. From a local preview with the same version number as the published installer, verify the panel offers **Install published build** when the executable checksum differs. Check that the request starts on click and sends only the information needed to retrieve public release metadata. Local settings, paths, ComfyUI addresses, and tokens stay on the PC.
6. Confirm **Install update** on the test profile. Check that the panel downloads and verifies the official Windows installer before replacing the installed version. Confirm that the existing installation remains usable after an unsigned installer, an altered download, or an interrupted download. After a successful update, reopen from Start and verify the new version, saved settings, token, and sign-in preference.
7. Test the update with work in the queue and with the panel's Render Bridge session active. Verify that the panel protects running and queued jobs and handles its own session before replacing files. Confirm that independently launched Render Bridge and ComfyUI processes keep running.
8. Install into a custom directory. Exit the tray, stop its Bridge server, and repair in place after removing a runtime program file in the clean test profile. Uninstall from a standalone copy of the wizard. Verify it removes its owned app files and matching shortcuts, preserves user settings and assets, and can reinstall in the same directory.

Use the Windows SDK's x64 SignTool and the release signing identity. Set `$SigningThumbprint` to its certificate thumbprint and `$TimestampUrl` to the signing provider's RFC 3161 endpoint. Copy the release executable to `RenderBridgeSetup.exe`, sign it, then verify its signature before calculating the checksum.

```powershell
signtool sign /sha1 $SigningThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 RenderBridgeSetup.exe
signtool verify /pa /v RenderBridgeSetup.exe
Get-FileHash -Algorithm SHA256 RenderBridgeSetup.exe
```

Create a draft GitHub release and attach the signed executable, its checksum, and third-party notices. Test the signed download on a clean Windows profile and confirm the expected publisher in Windows file properties. Check every draft asset before publishing. GitHub locks the release tag and assets on publication. Keep the current Windows download and README link active while signing or clean-profile verification is pending. Once the signed release is published, update the README to link to its versioned asset. [Microsoft SignTool documentation](https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool) · [GitHub immutable releases](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/establish-provenance-and-integrity/prevent-release-changes)

## macOS and Linux

Run `cargo build --release --locked --manifest-path installer/Cargo.toml` on the target platform.

Build on Apple silicon macOS and test the packaged app from Finder and Terminal. Verify the optional `.command` shortcut and wizard-owned process shutdown. Use Developer ID signing and notarization for a public macOS download. [Apple distribution guidance](https://developer.apple.com/developer-id/)

Build the Linux x64 installer against the oldest distribution you plan to support. Test startup, the folder chooser, the application-menu shortcut, and shutdown in a normal desktop session. Record the distro, display session, and runtime-library requirements with the asset. Include a checksum and third-party notices.
