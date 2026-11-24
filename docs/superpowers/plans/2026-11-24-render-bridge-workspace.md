# Render Bridge workspace implementation plan

## Goal

Bring the approved Render Bridge concept to the installer, tray, and desktop workspace. Start with model browsing, pipeline revisions, node inspection, and editable themes.

## Design

Keep Jobs compact for use from the tray. Use a larger resizable workspace for Models, Pipelines, and Nodes. Keep the existing black and dark teal surfaces and use purple for selection and primary actions. Mint indicates readiness, amber marks warnings, and red marks destructive actions. Dark, Light, and System modes share the same semantic color roles.

Model files, pipeline revisions, theme palettes, and node packages are separate resources. A pipeline revision can reference files and node types. A new render can use the copied options from a revision and running jobs keep their original inputs. Theme plugins are local JSON palettes with a versioned schema. They contain colors and a name and run no code.

## First implementation

- [x] **Theme system.** Centralize semantic color roles in `installer/src/theme.rs`. Validate a bounded, versioned JSON palette with optional dark and light overrides. Save it atomically per installation and expose import, color editing, export, and reset in Windows Settings. Keep automatic theme switching and legible action labels. Test local import, malformed palettes, reset, and system appearance.
- [x] **Models.** Rework `installer/src/model_ui.rs` into Installed, Packs, and Downloads with search and category filters where relevant. Reuse the existing download engine, registered storage locations, digest validation, and explicit repair. Keep downloads visible as users switch browsing tabs.
- [x] **Pipelines.** Add a workspace with a Wan Animate 2 graph, selectable nodes, and a swap inspector. Review Distilled and Full quality as known variants of this pipeline. Show weight, LoRA, and sampler changes while keeping shared encoders and the VAE visible. Check dependencies against the selected ComfyUI build. Save a validated local revision and copy its agent arguments.
- [x] **Nodes.** Add a searchable inventory using the selected ComfyUI build's node descriptions. Group known built-in and custom nodes. Show unreported origins separately. Keep inventory inspection read-only and surface source or version only if the build reports it.

The first implementation passed the Windows release build, 115 Rust tests, and 60 Python tests. The Windows tests used local fixtures, including the loader metadata for a backend without a model inventory route.

## Remaining app windows

- [x] **Workflow editing and node management.** Import and export ComfyUI API workflows. Map loaded model slots and graph ports, validate compatible alternatives, show dependency changes and required downloads, and add undo and a user-initiated test run. Let users inspect custom node sources and versions before approving an install. Keep existing saved workflows and submitted jobs stable.
- [ ] **Jobs and Connect.** Apply the new navigation and compact queue presentation to the existing panel. Verify live stage progress, start and stop controls, Gaming Mode, agent setup, token rotation, and a signed first-render receipt.
- [ ] **Settings and utilities.** Give Appearance, Storage, Advanced, and Diagnostics clear navigation. Apply the same styles to the tray menu, update and repair paths, uninstall, and confirmation dialogs. Keep file pickers and captions usable at small window sizes and high display scaling.
- [ ] **Installer.** Carry the same palette and navigation through Prerequisites, ComfyUI, Models, Install, and Connect. Verify fresh install and in-place upgrade with the optional model downloads, tray handoff, and agent connection.
- [ ] **Acceptance.** Run the Windows Rust and Python suites. Capture Dark and Light views of each screen at normal and high scaling. Exercise theme import and reset, downloads, node inventory, a pipeline revision, and restart persistence from a disposable installation.

The Windows workflow and node milestone passed 131 Rust tests and 72 Python tests. Native Dark and Light captures at 125% covered Models, built-in pipelines, the workflow editor, node inventory, and pack management with local fixture data. Workflow submission and pack management tests used isolated fixtures.

Desktop and terminal setup now accept a reusable settings JSON file. Existing saved values take precedence per field. Setup aborts before activation if the selected saved configuration changes while preparing the runtime.

## Verification boundaries

Confirm missing and unknown resources separately when the backend is offline or returns incomplete data. Display current state after a backend switch and discard results from an earlier backend. Invalid theme files leave the previous appearance active. Keep theme files and saved revisions independent of the server token and user settings. Verify agent-facing pipeline arguments against the shipped tool schema.
