# First-run readiness implementation plan

## Goal

Take a Windows user from a new installation to a downloadable first render and keep their MCP server available when setup closes.

## Approach

Use the installed panel as the Windows server owner from the first setup start. Keep server authentication, agent tool access, and a downloaded first render as separate checks. Reuse the existing catalog, download destinations, and signed file URLs.

- [x] **1. Match the video pack to the walkthrough.** The first-video walkthrough and release procedure use Wan Animate 2 with a character image, a short driving clip, and `animate_from_clip`. `animate_loop` and its Wan 2.2 downloads remain a separate pipeline. Source verification matched the catalog weights, arguments, and signed MP4 result field. Live playback is part of item 4.
- [x] **2. Make setup finish with a working agent connection.** Start and verify the server from the installed Windows panel before completing setup. Prove an existing listener uses the selected backend, asset folders, and installation token. Offer an explicit test from the user's agent and confirm the first render only after opening its signed 512 × 512 PNG. The native preview kept its original hidden tray and server processes across repeated starts on an isolated Windows desktop with a simulated notification area. Python fixtures checked ownership, mismatched settings, same-socket authentication, and invalid PNG receipts.
- [x] **3. Make model transfers recoverable.** Resolve writable registered folders per model category, check free space per volume, and let an interrupted pinned download resume. Validate the range and full SHA-256 before publishing. Distinguish listed files from verified files, and make repair an explicit choice that preserves the original until verification succeeds. Seventeen Windows model tests passed, including wrong ranges, corrupt content, changed repair targets, interruption, and existing files.
- [ ] **4. Run the exact release candidate.** Verify the signed executable and checksum on a clean Windows environment. Exercise Comfy Desktop and Portable, fresh Python installation, model download and recovery, an actual agent, and the setup-to-panel handoff. On an approved GPU host, download and open a PNG, the catalog MP4, and a GLB. Record the file digest, GPU, versions, and results in the release notes or PR evidence.

## Release dependencies

An authorized code-signing identity, a clean Windows environment, and an approved GPU run are needed for the last item. Fixture checks, the unsigned build, and the signed release procedure can be verified independently.
