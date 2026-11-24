# First-run readiness implementation plan

## Goal

Take a Windows user from a new installation to a downloadable first render and keep their MCP server available when setup closes.

## Approach

Use the installed panel as the Windows server owner from the first setup start. Keep server authentication, agent tool access, and a downloaded first render as separate checks. Reuse the existing catalog, download destinations, and signed file URLs.

- [x] **1. Match the video pack to the walkthrough.** The first-video walkthrough and release procedure use Wan Animate 2 with a character image, a short driving clip, and `animate_from_clip`. `animate_loop` and its Wan 2.2 downloads remain a separate pipeline. Source verification matched the catalog weights, arguments, and signed MP4 result field. The GPU test below decoded the resulting MP4.
- [x] **2. Make setup finish with a working agent connection.** Start and verify the server from the installed Windows panel before completing setup. Prove an existing listener uses the selected backend, asset folders, and installation token. Offer an explicit test from the user's agent and confirm the first render only after opening its signed 512 × 512 PNG. The native preview kept its original hidden tray and server processes across repeated starts on an isolated Windows desktop with a simulated notification area. Python fixtures checked ownership, mismatched settings, same-socket authentication, and invalid PNG receipts.
- [x] **3. Make model transfers recoverable.** Resolve writable registered folders per model category, check free space per volume, and let an interrupted pinned download resume. Validate the range and full SHA-256 before publishing. Distinguish listed files from verified files, and make repair an explicit choice that preserves the original until verification succeeds. Seventeen Windows model tests passed, including wrong ranges, corrupt content, changed repair targets, interruption, and existing files.
- [ ] **4. Verify the Windows release.**
  - [x] **Run real GPU output tests.** On November 24, 2026, a headless MCP client used the server runtime embedded in the unsigned Windows x64 0.2.0 preview, SHA-256 `2a89f1cfc083d8ebac951555f42be4d6c6a7a540ebe843b8c5de838305ed14cc`. The backend was ComfyUI 0.45.0 on an RTX 5090. Each file was downloaded through a signed Render Bridge link.
    - **PNG, Z-Image Turbo.** Opened at 512 × 512 and passed the installer's first-render receipt verifier.
    - **MP4, Wan Animate 2 distilled.** A signed upload supplied the driving clip. All 13 distinct H.264 frames decoded successfully at 320 × 320.
    - **GLB, Hunyuan 3D 2.1.** Headless Blender imported the untextured mesh with 1,923 faces.
  - [ ] **Sign and test the exact release asset last.** Sign and timestamp the executable, verify Authenticode, and calculate its SHA-256. Use that same file on clean Windows x64 to check Python installation, Comfy Desktop and Portable separately, model download and recovery, agent connection after setup closes, and upgrade, repair, and uninstall. Record the signed file digest and results in the release notes or PR evidence. [Release checklist](../../releases.md)

## Release dependencies

The final release check needs an authorized code-signing identity and a clean Windows x64 environment.
