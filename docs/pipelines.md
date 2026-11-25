# Models, nodes, and pipelines

Render Bridge turns your agent's requests into ComfyUI jobs. A **model** is a weight file ComfyUI loads. A **node** is an operation in the graph, such as loading a model, sampling an image, or saving the output. A **pipeline** is the graph that connects those operations. Render Bridge builds graphs for its named tools and can also submit a workflow you export.

## Accounts and credentials

| Account or credential | When you need it |
| --- | --- |
| Hugging Face account | The public, ungated starter downloads below work anonymously. For a gated model, sign in, request or accept access on its model page, and wait if approval is manual. For a private repo, first make sure its owner or organization has granted your account access. Download while signed in. [Hugging Face gated models](https://huggingface.co/docs/hub/en/models-gated#access-gated-models-as-a-user) |
| Hugging Face CLI sign-in | Optional for authenticated terminal downloads. Install the [Hugging Face CLI](https://huggingface.co/docs/huggingface_hub/main/guides/cli) separately and run `hf auth login` interactively. Use a read-scoped token if you choose token sign-in. |
| Your agent's usual login | Sign in to the MCP client you choose as you normally would. [Claude Code authentication](https://code.claude.com/docs/en/authentication) · [Codex authentication](https://learn.chatgpt.com/docs/auth#openai-authentication) |
| Render Bridge bearer token | The bridge generates this on the host. Put it in your agent's private MCP settings as described in the [README](../README.md#connect-an-mcp-client). Keep model-host and agent-provider credentials in their own tools. |
| Hosted or paid node credentials | The local pipelines here use installed weights and run without a Comfy account. A workflow using ComfyUI Partner Nodes may require a Comfy account and credits, or provider-specific sign-in. Verify those workflows in ComfyUI first. Render Bridge's `run_workflow` accepts the graph and has no provider-auth input. [ComfyUI Partner Nodes](https://docs.comfy.org/tutorials/partner-nodes/overview) |

Model weights and custom nodes have their own terms. Check the model card and node repository for the exact files you install.

## Download models in Render Bridge

Open **Models** in setup or in the installed panel. **Packs** in the panel and **Download** in setup list curated file sets for Z-Image Turbo stills, BiRefNet cutouts, and Wan Animate 2 video. They show each pack's terms, missing download size, and destination before you start. **Installed** shows the files reported by the current ComfyUI build, including manually added models. The panel's **Downloads** tab shows transfer progress.

Render Bridge chooses a registered writable folder for each model category and checks space on each destination drive. **Pause download** keeps completed files and partial data. Select **Download missing files** again to resume the same catalog version. Render Bridge checks the full size and SHA-256 before making a downloaded file available. The library refreshes after the transfer, and your agent can use `list_models` to see which pipelines are ready.

To check files already on disk, including manually added copies, choose **Verify installed files** on the pack. **Review repair** lists catalog files that failed verification. An approved repair replaces those files with verified copies and keeps the originals in a backup beside their model folder.

Public catalog files download anonymously. For a file that requires access, accept its terms on Hugging Face and optionally set a read-scoped `HF_TOKEN` in the environment that launches Render Bridge. Reopen Render Bridge from that environment so it can read the token. The app uses it only when Hugging Face asks for access. [Hugging Face token configuration](https://huggingface.co/docs/huggingface_hub/en/package_reference/environment_variables#hftoken)

Use the manual pipeline steps below for models and workflows outside the catalog.

## Recommended first pipeline

Start with **Z-Image Turbo**, the default `generate_image` preset (`z_image_turbo`). It uses ComfyUI core nodes and makes a good first test before adding editing, transparency, or video. The three weights below total about 15 GB of downloads. Disk size and GPU memory use are separate.

| Download | Place under your ComfyUI model root |
| --- | --- |
| [`SwarmUI_Z-Image-Turbo-FP8Mix.safetensors`](https://huggingface.co/mcmonkey/swarm-models/blob/main/SwarmUI_Z-Image-Turbo-FP8Mix.safetensors) | `models/diffusion_models/` |
| [`qwen_3_4b.safetensors`](https://huggingface.co/Comfy-Org/Z-Image-Turbo/blob/main/split_files/text_encoders/qwen_3_4b.safetensors) | `models/text_encoders/` |
| [`ae.safetensors`](https://huggingface.co/Comfy-Org/Z-Image-Turbo/blob/main/split_files/vae/ae.safetensors) | `models/vae/` |

Keep those filenames. The named preset looks up the listed FP8 checkpoint. For a different Z-Image variant, use a ComfyUI workflow that already selects that variant and submit it with [`run_workflow`](#run-a-workflow-from-comfyui).

1. Put the files in the directories used by the ComfyUI instance Render Bridge connects to. If your install uses extra model locations, use its [configured model search paths](https://docs.comfy.org/installation/manual_install#adding-extra-model-paths).
2. Refresh the model lists in ComfyUI or restart it after installing weights or nodes. Ask your agent to call `list_models` and look for `z_image_turbo` with `available: true`. If it reports an error, match the missing loader filename to the table.
3. Try one 512 × 512 image with the [first-render prompt](../README.md#make-your-first-render). Start with a batch of one and leave transparency off.

`list_models` checks filenames reported by ComfyUI's loaders. A listed preset still needs compatible graph nodes and enough memory to run.

For a one-file illustration starter, choose `anime` instead. Put [`NetaYumev35_pretrained_all_in_one.safetensors`](https://huggingface.co/duongve/NetaYume-Lumina-Image-2.0/blob/main/NetaYumev35_pretrained_all_in_one.safetensors) in `models/checkpoints/` and ask your agent to use that preset after `list_models` shows it is available.

## Store models on another drive

In **Models → Model storage and Hugging Face**, automatic storage uses the model paths the active build reports. To use an already configured model drive, choose its **Model storage folder**. If you are adding a new drive to ComfyUI, use **Copy model paths** to prepare its configuration. The copied YAML uses the selected folder as the base and expects category subfolders such as `diffusion_models`, `text_encoders`, and `vae` inside it.

- **ComfyUI portable or manual install.** Merge the copied entry into `extra_model_paths.yaml` in the ComfyUI directory. If you only have `extra_model_paths.yaml.example`, use it as a starting point for the new file. Keep existing entries and use a unique name for the new block. [ComfyUI extra model paths](https://docs.comfy.org/installation/manual_install#adding-extra-model-paths) · [Upstream YAML example](https://github.com/Comfy-Org/ComfyUI/blob/master/extra_model_paths.yaml.example)
- **Comfy Desktop.** Open the build's **Folders** tab and use **Add models folder** to select the external model folder. A build with a YAML entry also offers **Show extra_model_paths.yaml** to locate its file. Merge any YAML entries alongside the build's existing settings. [Comfy Desktop build folders](https://docs.comfy.org/installation/desktop/usage/instance-management#choose-which-folders-the-build-uses)

The current installer includes the advanced categories below. If you copied YAML from an earlier build, add any missing lines under the same `base_path` entry to store those weights on the external drive.

```yaml
    clip_vision: clip_vision/
    background_removal: background_removal/
    geometry_estimation: geometry_estimation/
```

Restart the selected ComfyUI build after saving the paths. Use **Refresh** on the **Models** page or call `list_models` from your agent to confirm the model filenames are available.

## Review pipelines and nodes

Open **Pipelines**, or choose **Use in pipeline** on the Wan Animate 2 pack. Open any graph step to see its menu. The **Motion model** menu switches between **Distilled** and **Full quality**. Full quality uses another diffusion weight, the Lightx2v LoRA, and a different step count. The **Character**, **Driving clip**, **Animate**, and **Save video** menus control image source, the frame rate for image sequences, render size, and contact-sheet preview. **VAE decode** shows the VAE used by the built-in preset. Validate the required models and nodes before saving a revision, then use **Copy agent options** to give your agent the selected settings for a new `animate_from_clip` call.

Open **Nodes** to search the selected build by name, ID, or category. Filter by built-in, custom, or unreported origin. If the backend reports a source or version, the selected node shows it. Follow the next section to manage custom nodes.

### Edit a ComfyUI workflow

1. Export the graph from ComfyUI in **API Format**. In Render Bridge, open **Pipelines → Your pipelines → Add pipeline**, import the JSON, and give the pipeline a name. Choose **Duplicate current** to start another pipeline from the open draft.
2. Open a saved pipeline from the library. Render Bridge checks the graph against the selected ComfyUI build. Open a step and choose an input to change its value, choose one of its reported models, or connect a compatible output. Use **Check dependencies** to refresh after adding models or nodes.
3. To start from a model, open **Models → Installed**, select a file, and choose **Use in pipeline**. Open the pipeline you want, choose one of the compatible loader slots, and select **Apply model**. The selected build supplies the choices for that slot.
4. Select **Save changes** to keep the edits in your local pipeline library. **Undo** reverses the last edit. **Export copy** writes a separate API JSON file. **Copy agent prompt** gives a connected agent the current graph for a new `run_workflow` job.

**Test run** explicitly queues a new job on the selected local ComfyUI instance. The app checks the graph again at submission. Gaming Mode must be off. The test accepts built-in local ComfyUI nodes and requires you to confirm before it runs. Watch queue activity in **Jobs**. Copy the prompt ID to look up this test in ComfyUI. Custom and hosted workflows should be tested in ComfyUI first. Editing or exporting a draft leaves queued and running jobs alone.

### Add a pipeline with an agent

In Render Bridge, open **Pipelines → Your pipelines → Add pipeline**. Choose **Copy pipeline setup prompt** and paste it into your agent. Describe what you want to make and share an existing workflow if you have one. The agent can also follow this section directly.

If the connected MCP server exposes `add_pipeline`, pass the new pipeline's `name` and ComfyUI API-format graph as `workflow`. The tool saves it to **Your pipelines** and returns its ID, missing models and nodes, and dependencies it could not verify. Use a unique name. A saved draft can have missing dependencies. To render it, submit the graph with `run_workflow` when you are ready.

Use these steps when the connected server does not expose `add_pipeline`.

1. Ask the user for the intended output and any workflow they want to build on. Prepare a ComfyUI API-format graph as a JSON object keyed by node ID. Each node has a `class_type` and an `inputs` object. A link such as `["12", 0]` uses output 0 from node 12. Include the output node that saves the intended result.
2. Use read-only checks where available. Call Render Bridge's `list_models` tool and inspect the [ComfyUI node registry](https://docs.comfy.org/development/comfyui-server/comms_routes) on the target build if you have access to it. Report any missing or unverified model files and node packs. Get approval before installing dependencies or replacing files.
3. Save a new `.json` file somewhere the user can open it, or give them the JSON to save. Have the user return to **Add pipeline**, choose **Import API JSON**, name the pipeline, and select **Add to library**. In the saved pipeline, **Check dependencies** checks the selected build again.

The Render Bridge app imports the file into its local library. The MCP tool `run_workflow` submits a render job, so wait for the user's approval before using it.

## Check and install nodes

Standard generation, Qwen image tools, and BiRefNet cutouts use nodes supplied with current ComfyUI. The named Wan and 3D pipelines also use newer core nodes. Start by updating ComfyUI if a core node is missing. Examples include `RemoveBackground`, `WanAnimate2ToVideo`, `WanFirstLastFrameToVideo`, `LoadMoGeModel`, and `Save3DAdvanced`.

In **Nodes**, inspect the selected build's built-in and custom node inventory. In **Manage packs**, enter the public GitHub repository and its full commit SHA. Review the pinned source and dependency files before approving installation into the displayed local ComfyUI directory. Render Bridge stages and copies the selected archive. It leaves Python dependency installation and server restart to you. On restart, ComfyUI imports enabled custom nodes as Python code, so review the source and follow the author's installation instructions first.

Disable or re-enable packs installed through Render Bridge from the same tab. Disabling requires a verified idle queue and moves its files out of the active `custom_nodes` directory and takes effect the next time ComfyUI loads nodes. Packages already installed through another tool stay under that tool's management.

For another source or a workflow that needs ComfyUI Manager, open it in ComfyUI and use [ComfyUI Manager's missing-node flow](https://docs.comfy.org/installation/install_custom_node) or follow the node author's instructions. Install only packs you trust into the ComfyUI environment, restart ComfyUI, and run the graph in its UI before using Render Bridge. A node's API class can also be checked via `GET /object_info/<class_type>` on the local ComfyUI API. [ComfyUI API routes](https://docs.comfy.org/development/comfyui-server/comms_routes)

## What each pipeline does

| Goal | Render Bridge tool | Install or prepare |
| --- | --- | --- |
| Create a still image from text | `generate_image` | Start with [Z-Image Turbo](#recommended-first-pipeline). Try [Qwen-Image](#qwen-image-generation-and-editing) for another text-to-image option. |
| Edit an image or repaint a sequence of frames | `edit_image`, `repaint_frames` | Install the [Qwen editing files](#qwen-image-generation-and-editing). |
| Cut out a subject or generate a transparent PNG | `remove_background` or `transparent=true` | Install [BiRefNet](#transparent-images-and-cutouts) and its core ComfyUI nodes. |
| Transfer a clip's motion to a character image | `animate_from_clip` | Download **Wan Animate 2** in **Models**. [First video](#make-your-first-video) |
| Make a loop from one image | `animate_loop` | Install the separate [Wan 2.2 loop weights](#make-a-loop-from-one-image). |
| Generate a GLB from a single image | `image_to_3d` | Set up the [3D model files and workflow](#image-to-3d). |
| Submit a graph you build yourself | `run_workflow` | [Export it in API format](#run-a-workflow-from-comfyui) after testing it in ComfyUI. |

For model-heavy operations that support it, call `start_job`, then `get_job` to retrieve the finished result. Keep the job ID and returned file references together.

After a Bridge restart, call `get_job` with the same ID. Recent completed jobs return fresh links in `outputs`. The Bridge keeps up to 256 recent inactive job records for up to 24 hours. A job that was in flight can report `recovering` while ComfyUI finishes its existing prompts, then `interrupted` with any partial outputs found. To cancel, call `cancel_job` and poll `get_job`. `cancel_requested` means ComfyUI acknowledged the request, `cancelled` means the Bridge confirmed the stop, and `cancel_unknown` means it could not confirm the outcome. Running-job cancellation depends on ComfyUI's prompt-specific cancel support.

For an image already generated on this server, give the editing or cutout tool its returned `filename` and `subfolder` with `source="output"`. For a file on your own device, ask your agent to use `upload_link`, upload the file, then pass the returned file reference with `source="input"`.

### Transparent images and cutouts

Place [`birefnet.safetensors`](https://huggingface.co/Comfy-Org/BiRefNet/blob/main/background_removal/birefnet.safetensors) in `models/background_removal/`. The bridge uses ComfyUI's `LoadBackgroundRemovalModel` and `RemoveBackground` nodes to create a mask and save RGBA PNGs. Use `remove_background` on an existing image, or pass `transparent=true` to supported tools. The animation tools and `repaint_frames` ask for transparency by default. Pass `transparent=false` if you do not want that step. [ComfyUI's BiRefNet guide](https://docs.comfy.org/tutorials/utility/remove-background-birefnet)

### Qwen image generation and editing

Install the shared encoder and VAE first. Then choose generation, editing, or both. Qwen weights are substantially larger than the Z-Image Turbo starter.

| Use | Download | Folder under `models/` |
| --- | --- | --- |
| Shared | [`qwen_2.5_vl_7b_fp8_scaled.safetensors`](https://huggingface.co/Comfy-Org/Qwen-Image/blob/main/split_files/text_encoders/qwen_2.5_vl_7b_fp8_scaled.safetensors) | `text_encoders/` |
| Shared | [`qwen_image_vae.safetensors`](https://huggingface.co/Comfy-Org/Qwen-Image/blob/main/split_files/vae/qwen_image_vae.safetensors) | `vae/` |
| `qwen_image` or `qwen_image_fast` | [`qwen_image_2512_fp8_e4m3fn.safetensors`](https://huggingface.co/Comfy-Org/Qwen-Image/blob/main/split_files/diffusion_models/qwen_image_2512_fp8_e4m3fn.safetensors) | `diffusion_models/` |
| `qwen_image_fast` | [`Qwen-Image-2512-Lightning-4steps-V1.0-bf16.safetensors`](https://huggingface.co/lightx2v/Qwen-Image-2512-Lightning/blob/main/Qwen-Image-2512-Lightning-4steps-V1.0-bf16.safetensors) | `loras/` |
| `edit_image` or `repaint_frames` | [`qwen_image_edit_2511_fp8mixed.safetensors`](https://huggingface.co/Comfy-Org/Qwen-Image-Edit/blob/main/split_files/diffusion_models/qwen_image_edit_2511_fp8mixed.safetensors) | `diffusion_models/` |
| `edit_image` or `repaint_frames` | [`Qwen-Image-Edit-2511-Lightning-4steps-V1.0-bf16.safetensors`](https://huggingface.co/lightx2v/Qwen-Image-Edit-2511-Lightning/blob/main/Qwen-Image-Edit-2511-Lightning-4steps-V1.0-bf16.safetensors) | `loras/` |

The editing tools check for all four editing files, including the editing LoRA, even if you choose `fast=false`. The generation and editing Lightning files are separate downloads. For `repaint_frames`, provide the frames in order at a consistent size. Use [BiRefNet](#transparent-images-and-cutouts) if the result should have an alpha channel.

### Animation

Video files and runtime memory requirements are larger than the still-image starter.

#### Make your first video

1. Download **Wan Animate 2** in **Models**. The pack supplies the weights for the default mode of `animate_from_clip`.
2. Choose a reference image of your character and a short MP4 or MOV that shows the motion you want to transfer. Keep the framing similar in both. Start with a clip of no more than 33 frames.
3. Give both inputs to your connected agent. It can reuse an image Render Bridge generated by passing the returned file reference with `source="output"`. For local inputs, ask it to upload each file through `upload_link` and use the returned file references with `source="input"`.

> Use my reference image and driving clip to run `animate_from_clip` through `start_job`. Describe the character from the reference image, set `quality=false` and `transparent=false`, and use `get_job` to wait for the result. Give me the signed `video.url` for the completed MP4. If a prerequisite is missing, tell me which one.

Download and play the MP4 from that link. Use a link reachable from the device where you open it. [File transfer and signed links](connections.md#verify-access-and-transfer-files)

For manual installation, use the following [Wan Animate 2 files](https://huggingface.co/Comfy-Org/Wan-Animate-2). The [official distilled template](https://raw.githubusercontent.com/Comfy-Org/workflow_templates/main/templates/video_wan_animate2_distilled.json) has the download locations.

| Folder under `models/` | Filename |
| --- | --- |
| `diffusion_models/` | `wan_animate_2_distill_int8_convrot.safetensors` |
| `text_encoders/` | `umt5_xxl_fp8_e4m3fn_scaled.safetensors` |
| `vae/` | `Wan2_1_VAE_bf16.safetensors` |
| `clip_vision/` | `clip_vision_h.safetensors` |

You can supply an ordered set of equal-size frames in place of the driving video. Video input is limited to 64 MiB, with a cap of 49 frames and a lower frame budget at larger output sizes.

For `quality=true`, use `wan_animate_2_int8_convrot.safetensors` in `diffusion_models/` and add `lightx2v_I2V_14B_480p_cfg_step_distill_rank64_bf16.safetensors` in `loras/`. See the [quality template](https://github.com/Comfy-Org/workflow_templates/blob/main/templates/video_wan_animate2.json).

#### Make a loop from one image

`animate_loop` makes a short loop from one image using a separate Wan 2.2 weight set. Follow the links in the [Wan 2.2 first/last-frame template](https://github.com/Comfy-Org/workflow_templates/blob/main/templates/video_wan2_2_14B_flf2v.json) for these exact files.

| Folder under `models/` | Filename |
| --- | --- |
| `diffusion_models/` | `wan2.2_i2v_high_noise_14B_fp8_scaled.safetensors`, `wan2.2_i2v_low_noise_14B_fp8_scaled.safetensors` |
| `text_encoders/` | `umt5_xxl_fp8_e4m3fn_scaled.safetensors` |
| `vae/` | `wan_2.1_vae.safetensors` |
| `loras/` (fast mode) | `wan2.2_i2v_lightx2v_4steps_lora_v1_high_noise.safetensors`, `wan2.2_i2v_lightx2v_4steps_lora_v1_low_noise.safetensors` |

The default fast mode needs both LoRAs. `quality=true` skips them. The product of `frames` and `spacing` must be divisible by four and at most 48. For both Wan tools, install [BiRefNet](#transparent-images-and-cutouts) for transparent frames or pass `transparent=false`.

### Image to 3D

1. Download the official [Pixal3D and TRELLIS.2 template JSON](https://raw.githubusercontent.com/Comfy-Org/workflow_templates/main/templates/3d_pixal3d_trellis2_image_to_model.json) and open it in current ComfyUI. Install the models specified by the template for the branches you intend to run.
2. Test the chosen branch in ComfyUI, then save its **Export (API)** JSON locally. Use a `Save3DAdvanced` node with its `viewport_state` intact and a workflow that saves a GLB.
3. Stop Render Bridge and set `COMFY_3D_WORKFLOW` to the export's absolute path in the environment that starts the Bridge. On Windows, use **Connect → Shutdown and terminal commands → Copy start command** in Setup. Set the variable in PowerShell, then run the copied start command in that same PowerShell window and leave it running.

```powershell
$env:COMFY_3D_WORKFLOW = (Resolve-Path -LiteralPath 'C:\RenderBridge\workflows\image-to-3d-api.json').Path
```

Replace the example path with your saved export. In the connected agent, call `list_models` and check `image_to_3d_configured`. If it is false, read `image_to_3d_error` for guidance. That check covers loading the graph. Test the actual render with `image_to_3d` to verify the installed nodes, weights, and hardware.

For the full two-model template, use the file links in the template to install the following weights.

| Folder under `models/` | Filename |
| --- | --- |
| `diffusion_models/` | `pixal3d_int8_convrot.safetensors`, `trellis_2_int8_convrot.safetensors` |
| `clip_vision/` | `dino_v3_L_naf_fp32.safetensors` |
| `vae/` | `trellis_2_shape_vae_bf16.safetensors`, `trellis_2_texture_vae_bf16.safetensors` |
| `geometry_estimation/` | `moge_2_vitl_normal_fp16.safetensors` |
| `background_removal/` | `birefnet.safetensors` if using background removal |

Pixal3D and TRELLIS.2 produce textured GLBs. The optional Hunyuan 3D 2.1 path uses `models/checkpoints/hunyuan_3d_v2.1.safetensors` and the same workflow file for the save viewport. It produces an untextured GLB. Check model terms before using these outputs, including [DINOv3's license](https://raw.githubusercontent.com/facebookresearch/dinov3/main/LICENSE.md) with the Pixal/TRELLIS pipeline and [Hunyuan's territorial terms](https://github.com/Tencent-Hunyuan/Hunyuan3D-2.1/blob/main/LICENSE) if you choose Hunyuan.

`optimize_glb` is optional. Install Node.js and the [glTF Transform CLI](https://gltf-transform.dev/cli), put `node` on PATH, then set `GLTF_TRANSFORM_CLI` to the installed package's JavaScript entry point (`bin/cli.js`). Use an input GLB accessible to the bridge.

### Run a workflow from ComfyUI

1. Open a trusted workflow in ComfyUI. Resolve any missing nodes and weights, set its inputs, and run it once in the UI. Include a save node such as `SaveImage` so the bridge has a persisted file to return.
2. Use **Export (API)** in ComfyUI. The result should be an object keyed by node ID, with `class_type` and `inputs` for each node.
3. If the graph uses local images and your agent is on another machine, use `upload_link` to stage them on the Render Bridge host. Use the returned filenames and subfolders in the graph inputs.
4. Ask your agent to pass the API graph object to `run_workflow`. It can return links to saved images, videos, or models reported by the workflow. Use `start_job` and `get_job` if the workflow will take a while.
