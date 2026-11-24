"""Expose an independently managed ComfyUI instance as a Streamable HTTP MCP server.

ComfyUI and this server share access to the same input and output directories.
MCP requests use bearer authentication and file transfers use signed URLs.
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import contextvars
import hashlib
import hmac
import inspect
import ipaddress
import io
import json
import math
import os
import random
import re
import secrets
import socket
import shutil
import stat
import time
import uuid
from functools import cache
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, quote, unquote, urlencode, urlsplit

import httpx
import uvicorn
from PIL import Image as PILImage
from PIL import ImageChops, ImageFilter, ImageOps
from mcp.server.mcpserver import MCPServer
from mcp.server.mcpserver.exceptions import ToolError
from mcp_types import CallToolResult, ImageContent, TextContent, ToolAnnotations
from starlette.responses import JSONResponse, PlainTextResponse, Response, StreamingResponse

if os.name == "nt":
    USER_DATA_ROOT = Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local")
else:
    USER_DATA_ROOT = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share")

DATA_DIR = Path(os.environ.get("COMFY_MCP_DATA_DIR", USER_DATA_ROOT / "comfy-mcp")).expanduser()
COMFY_URL = os.environ.get("COMFY_URL", "http://127.0.0.1:8188").rstrip("/")
MCP_HOSTS = [host.strip() for host in os.environ.get("MCP_HOST", "127.0.0.1").split(",") if host.strip()]
MCP_PORT = int(os.environ.get("MCP_PORT", "8189"))
PUBLIC_URLS = {
    label.strip(): url.strip().rstrip("/")
    for label, url in (
        entry.split("=", 1)
        for entry in os.environ.get("PUBLIC_URLS", f"local=http://127.0.0.1:{MCP_PORT}").split(",")
        if "=" in entry
    )
    if label.strip() and url.strip()
}
PUBLIC_URL = next(iter(PUBLIC_URLS.values()), "")
LINK_TTL_S = float(os.environ.get("DOWNLOAD_LINK_DAYS", "1")) * 86400
TOKEN_FILE = Path(os.environ.get("MCP_TOKEN_FILE", DATA_DIR / "token.txt")).expanduser()
DEFAULT_TIMEOUT_S = int(float(os.environ.get("JOB_TIMEOUT_MIN", "15")) * 60)
QUEUE_WAIT_MAX_S = float(os.environ.get("QUEUE_WAIT_MAX_MIN", "240")) * 60
IDLE_UNLOAD_S = float(os.environ.get("IDLE_UNLOAD_MIN", "10")) * 60
COMFY_BUSY_WAIT_S = float(os.environ.get("COMFY_BUSY_WAIT_S", "60"))
PAUSE_FLAG = Path(os.environ.get("PAUSE_FLAG", DATA_DIR / "gaming_mode.flag")).expanduser()
PAUSED_MSG = "Gaming mode is on. ComfyUI MCP is not accepting new GPU work. Retry when gaming mode is off."
UPLOAD_SUBFOLDER = "uploads"
UPLOAD_MAX_BYTES = int(float(os.environ.get("UPLOAD_MAX_MB", "2048")) * 1024 * 1024)


def is_loopback(host: str) -> bool:
    if host.lower() == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def validate_server_settings() -> None:
    if not math.isfinite(IDLE_UNLOAD_S) or IDLE_UNLOAD_S < 0:
        raise SystemExit("Set IDLE_UNLOAD_MIN to a finite non-negative number of minutes.")
    if not MCP_HOSTS or not 1 <= MCP_PORT <= 65535:
        raise SystemExit("Set MCP_HOST and MCP_PORT to valid listen addresses and a port.")
    if any(not is_loopback(host) for host in MCP_HOSTS):
        if os.environ.get("MCP_ALLOW_REMOTE") != "1":
            raise SystemExit("Set MCP_ALLOW_REMOTE=1 to listen beyond loopback. Use a private tunnel or TLS proxy.")
        if "PUBLIC_URLS" not in os.environ:
            raise SystemExit("Set PUBLIC_URLS to the client-visible address for signed transfers.")
    if not PUBLIC_URLS:
        raise SystemExit("Set PUBLIC_URLS as label=http[s]://host[:port].")
    for url in PUBLIC_URLS.values():
        parsed = urlsplit(url)
        if (parsed.scheme not in ("http", "https") or not parsed.hostname or parsed.username is not None
                or parsed.query or parsed.fragment):
            raise SystemExit("Each PUBLIC_URLS entry must be an HTTP(S) base URL without credentials or a query.")
    for path in COMFY_DIRS.values():
        if not path.is_dir():
            raise SystemExit(f"ComfyUI I/O directory is missing: {path}. Set COMFY_ROOT to its data directory.")


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


@cache
def load_token() -> str:
    TOKEN_FILE.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        fd = os.open(TOKEN_FILE, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        if os.name != "nt" and stat.S_IMODE(TOKEN_FILE.stat().st_mode) & 0o077:
            raise SystemExit("MCP token file must be private to its account. Set permissions to 600.")
        token = TOKEN_FILE.read_text(encoding="utf-8").strip()
        if not re.fullmatch(r"[A-Za-z0-9_-]{32,}", token):
            raise SystemExit("MCP token must contain at least 32 URL-safe letters, digits, hyphens, or underscores.")
        return token
    token = secrets.token_urlsafe(32)
    with os.fdopen(fd, "w", encoding="utf-8") as token_file:
        token_file.write(token + "\n")
    return token


def _link_sig(filename: str, subfolder: str, ftype: str, exp: int) -> str:
    msg = f"{ftype}|{subfolder}|{filename}|{exp}".encode()
    return hmac.new(load_token().encode(), msg, hashlib.sha256).hexdigest()[:40]


@cache
def _upload_key() -> bytes:
    return hmac.new(load_token().encode(), b"comfy-mcp upload links v1", hashlib.sha256).digest()


def _upload_sig(folder: str, filename: str, project: str, exp: int, nonce: str) -> str:
    msg = f"{folder}|{filename}|{project}|{exp}|{nonce}".encode()
    return hmac.new(_upload_key(), msg, hashlib.sha256).hexdigest()[:40]


# ---------------------------------------------------------------------------
# Presets (settings taken from the official ComfyUI workflow templates)
# ---------------------------------------------------------------------------
QWEN_DEFAULT_NEGATIVE = "低分辨率，低画质，肢体畸形，手指畸形，画面过饱和，蜡像感，人脸无细节，过度光滑，画面具有AI感。构图混乱。文字模糊，扭曲"
# Anima (from its model card): quality and safety tags help; score_* tags are left out because they push the
# Aesthetic and Turbo versions too hard. See _anima_tags.
ANIMA_QUALITY_TAGS = "masterpiece, best quality"
_ANIMA_SAFETY_TAG = re.compile(r"(^|,)\s*(safe|sensitive|nsfw|explicit)\s*(,|$)", re.IGNORECASE)
ANIMA_NEG_DEFAULT = "worst quality, low quality, blurry, jpeg artifacts, chromatic aberration, artist name, signature, watermark, nsfw, explicit"
SDXL_NEG_DEFAULT = (
    "worst quality, low quality, lowres, blurry, jpeg artifacts, watermark, signature, text, logo, deformed, "
    "bad anatomy, bad hands, extra limbs, extra fingers, cropped"
)

# Every preset names its model family. A style LoRA only works with the family it was trained for, so LoRA files
# live in models/loras/<family>/ and generate_image(loras=...) refuses one from another family.
PRESETS: dict[str, dict[str, Any]] = {
    "z_image_turbo": {
        "description": "Z-Image Turbo (Apache-2.0). Fast (8 steps), strong prompt following and text. Good default.",
        "family": "z_image",
        "unet": "SwarmUI_Z-Image-Turbo-FP8Mix.safetensors",
        "clip": "qwen_3_4b.safetensors",
        "clip_type": "lumina2",
        "vae": "ae.safetensors",
        "shift": 3.0,
        "steps": 8,
        "cfg": 1.0,
        "sampler": "res_multistep",
        "scheduler": "simple",
        "size": (1024, 1024),
        "negative": None,
        "lora": None,
    },
    "qwen_image": {
        "description": "Qwen-Image-2512 (Apache-2.0). Highest quality and best in-image text/UI lettering. 50 steps, slower.",
        "family": "qwen_image",
        "unet": "qwen_image_2512_fp8_e4m3fn.safetensors",
        "clip": "qwen_2.5_vl_7b_fp8_scaled.safetensors",
        "clip_type": "qwen_image",
        "vae": "qwen_image_vae.safetensors",
        "shift": 3.1,
        "steps": 50,
        "cfg": 4.0,
        "sampler": "euler",
        "scheduler": "simple",
        "size": (1328, 1328),
        "negative": QWEN_DEFAULT_NEGATIVE,
        "lora": None,
    },
    "qwen_image_fast": {
        "description": "Qwen-Image-2512 + Lightning 4-step LoRA (Apache-2.0). Near-Qwen quality in 4 steps.",
        "family": "qwen_image",
        "unet": "qwen_image_2512_fp8_e4m3fn.safetensors",
        "clip": "qwen_2.5_vl_7b_fp8_scaled.safetensors",
        "clip_type": "qwen_image",
        "vae": "qwen_image_vae.safetensors",
        "shift": 3.1,
        "steps": 4,
        "cfg": 1.0,
        "sampler": "euler",
        "scheduler": "simple",
        "size": (1328, 1328),
        "negative": None,
        "lora": ("Qwen-Image-2512-Lightning-4steps-V1.0-bf16.safetensors", 1.0),
    },
    "anime": {
        "description": "NetaYume Lumina v3.5 (Apache-2.0). Anime / illustration concept art, characters, key visuals. 30 steps.",
        "builder": "neta",
        "family": "neta_lumina",
        "ckpt": "NetaYumev35_pretrained_all_in_one.safetensors",
        "shift": 4.0,
        "steps": 30,
        "cfg": 4.0,
        "sampler": "res_multistep",
        "scheduler": "simple",
        "size": (1024, 1024),
        "negative": None,
        "lora": None,
    },
    "cinematic": {
        "description": "HiDream-O1 (MIT, 8B). Cinematic / Unreal-Engine-style renders, environments, hero shots. 40 steps, up to 2048px.",
        "builder": "hidream",
        "family": "hidream_o1",
        "ckpt": "hidream_o1_image_bf16.safetensors",
        "noise_scale": 8.0,
        "steps": 40,
        "cfg": 5.0,
        "sampler": "dpmpp_2m_sde_gpu",
        "scheduler": "normal",
        "size": (1536, 1536),
        "snap": 32,
        "negative": None,
        "lora": None,
    },
    "flux_dev": {
        "description": "FLUX.1-dev (licence: FLUX.1-dev Non-Commercial). Strong general / photoreal / painterly art; "
                       "takes the painterly FLUX.1 LoRAs (loras=...). 20 steps.",
        "builder": "flux",
        "family": "flux1",
        "ckpt": "flux1-dev-fp8.safetensors",
        "guidance": 3.5,
        "steps": 20,
        "cfg": 1.0,
        "sampler": "euler",
        "scheduler": "simple",
        "size": (1024, 1024),
        "negative": None,
        "lora": None,
    },
    "sd35": {
        "description": "Stable Diffusion 3.5 Large (licence: Stability AI Community, free under US$1M revenue). Wide style range, real negative prompts. 20 steps.",
        "builder": "sd35",
        "family": "sd35",
        "ckpt": "sd3.5_large_fp8_scaled.safetensors",
        "steps": 20,
        "cfg": 4.0,
        "sampler": "euler",
        "scheduler": "sgm_uniform",
        "size": (1024, 1024),
        "negative": None,
        "lora": None,
    },
    "anima_turbo": {
        "description": "Anima Turbo v1.1 (CircleStone Labs, 2B; non-commercial model licence, outputs usable commercially). "
                       "Fast anime / illustration / painterly art, not realism. Takes the Anima LoRAs. 10 steps, CFG 1.",
        "builder": "anima",
        "family": "anima",
        "unet": "anima-turbo-v1.1.safetensors",
        "clip": "qwen_3_06b_base.safetensors",
        "vae": "qwen_image_vae.safetensors",
        "steps": 10,
        "cfg": 1.0,
        "sampler": "euler",
        "scheduler": "simple",
        "size": (1024, 1024),
        "negative": ANIMA_NEG_DEFAULT,
        "lora": None,
    },
    "anima_aesthetic": {
        "description": "Anima Aesthetic v1.1 (CircleStone Labs, 2B; non-commercial model licence, outputs usable "
                       "commercially). Best-looking anime / illustration / painterly art, not realism. Takes the Anima "
                       "LoRAs. 30 steps, CFG 4.",
        "builder": "anima",
        "family": "anima",
        "unet": "anima-aesthetic-v1.1.safetensors",
        "clip": "qwen_3_06b_base.safetensors",
        "vae": "qwen_image_vae.safetensors",
        "steps": 30,
        "cfg": 4.0,
        "sampler": "euler",
        "scheduler": "simple",
        "size": (1024, 1024),
        "negative": ANIMA_NEG_DEFAULT,
        "lora": None,
    },
    "juggernaut_xl": {
        "description": "Juggernaut XI v11 (SDXL fine-tune by RunDiffusion; their terms bar rehosting it as a service). "
                       "Versatile SDXL workhorse: pair it with the painterly SDXL LoRAs (loras=...). 30 steps, CFG 4.5.",
        "builder": "sdxl",
        "family": "sdxl",
        "ckpt": "Juggernaut-XI-byRunDiffusion.safetensors",
        "steps": 30,
        "cfg": 4.5,
        "sampler": "dpmpp_2m",
        "scheduler": "karras",
        "size": (1024, 1024),
        "negative": SDXL_NEG_DEFAULT,
        "lora": None,
    },
    "flux3_klein": {
        "description": "FLUX.3 Image Klein (Black Forest Labs, distilled; FLUX Kommunity licence). The newest BFL model: "
                       "strong prompt following, in-image text and a wide range of art styles. 16 steps, no CFG.",
        "builder": "flux3",
        "family": "flux3_klein",
        "unet": "flux3-image-klein-bf16.safetensors",
        "clip": "qwen3vl_4b_int8_convrot.safetensors",
        "vae": "flux2-vae.safetensors",
        "steps": 16,
        "cfg": 1.0,
        "sampler": "euler",
        "scheduler": "simple",
        "size": (1024, 1024),
        "negative": None,
        "lora": None,
    },
    "flux3_klein_base": {
        "description": "FLUX.3 Image Klein Base (Black Forest Labs, undistilled; FLUX Kommunity licence). More varied "
                       "than flux3_klein and takes real negative prompts, but slower: 50 steps, CFG 3.5.",
        "builder": "flux3",
        "family": "flux3_klein",
        "unet": "flux3-image-klein-base-bf16.safetensors",
        "clip": "qwen3vl_4b_int8_convrot.safetensors",
        "vae": "flux2-vae.safetensors",
        "steps": 50,
        "cfg": 3.5,
        "sampler": "euler",
        "scheduler": "simple",
        "size": (1024, 1024),
        "negative": None,
        "lora": None,
    },
}

# Notes on the style LoRAs installed for agents, keyed by file name. list_models lists every file under
# models/loras/<family>/, noted here or not; a trigger is added to the prompt for you when it is missing.
LORA_NOTES: dict[str, dict[str, Any]] = {
    "anima-greg-rutkowski-style.safetensors": {
        "about": "Official Anima LoRA: painterly fantasy illustration in the style of Greg Rutkowski.", "strength": 1.0},
    "anima-highres-aesthetic-boost.safetensors": {
        "about": "Official Anima LoRA: sharper detail and a more polished finish.", "strength": 1.0},
    "watercolor_v1_sdxl.safetensors": {"about": "Watercolor painting style (ostris).", "strength": 1.0},
    "gouache-storybook.safetensors": {
        "about": "Gouache storybook illustration.", "trigger": "g0u4ch3", "strength": 0.9},
    "sdxl-oil-painting-oil-brush-stroke.safetensors": {
        "about": "Oil painting with visible brush strokes.", "trigger": "bichu, oil painting", "strength": 1.0},
    "hand-painted-2d-seamless-textures.safetensors": {
        "about": "Hand-painted 2D seamless textures (tiles for ground, walls, materials).",
        "trigger": "2d seamless hand-painted texture", "strength": 1.0},
    "DD-lclr-artstyle-XL-v1.safetensors": {
        "about": "Loose painterly artstyle (Elijah LaClair).", "trigger": "lclr artstyle, painting", "strength": 1.0},
    "midjourney-painterly.safetensors": {
        "about": "Rich painterly look; card suggests 30-35 steps.", "trigger": "mj painterly", "strength": 1.0},
    "flux-oilpainting1.3.safetensors": {
        "about": "Strongly textured oil painting; keep scenes simple or textures get busy.", "strength": 0.9},
    "flux-aquarell-watercolor-v2.safetensors": {
        "about": "Aquarelle watercolor; works best with 'white background'.", "trigger": "AQUACOLTOK", "strength": 1.0},
    "flux-watercolor-strokes.safetensors": {
        "about": "Watercolor with visible strokes.", "trigger": "WTRCLR, watercolor", "strength": 1.0},
    "flux-hand-painted-textures.safetensors": {
        "about": "Hand-painted 2D seamless textures.", "trigger": "2d seamless hand-painted texture of", "strength": 1.0},
}
MAX_LORAS = 4
HUNYUAN3D_CKPT = "hunyuan_3d_v2.1.safetensors"
PROVENANCE_DIR = Path(os.environ.get("PROVENANCE_DIR", DATA_DIR / "provenance")).expanduser()
# NetaYume Lumina expects these system-prompt prefixes (from the official ComfyUI template).
NETA_POS_SYSTEM = "You are an assistant designed to generate high quality anime images based on textual prompts. <Prompt Start> "
NETA_NEG_SYSTEM = "You are an assistant designed to generate low-quality images based on textual prompts <Prompt Start> "
NETA_NEG_DEFAULT = (
    "blurry, worst quality, low quality, jpeg artifacts, signature, watermark, username, error, deformed hands, "
    "bad anatomy, extra limbs, poorly drawn hands, poorly drawn face, mutation, deformed, extra eyes, extra arms, "
    "extra legs, malformed limbs, fused fingers, too many fingers, long neck, cross-eyed, bad proportions, "
    "missing arms, missing legs, extra digit, fewer digits, cropped"
)
# Qwen-Image-Edit-2511 (Apache-2.0); settings mirror ComfyUI's image_qwen_image_edit_2511 template.
EDIT_MODELS = {
    "unet": "qwen_image_edit_2511_fp8mixed.safetensors",
    "clip": "qwen_2.5_vl_7b_fp8_scaled.safetensors",
    "vae": "qwen_image_vae.safetensors",
    "lora": "Qwen-Image-Edit-2511-Lightning-4steps-V1.0-bf16.safetensors",
}
THREE_D_WORKFLOW = os.environ.get("COMFY_3D_WORKFLOW")
COMFY_ROOT = Path(os.environ.get("COMFY_ROOT", Path.home() / "ComfyUI")).expanduser()
COMFY_DIRS = {
    kind: Path(os.environ.get(f"COMFY_{kind.upper()}_DIR", COMFY_ROOT / kind)).expanduser()
    for kind in ("input", "output")
}
GLTF_TRANSFORM_CLI = os.environ.get("GLTF_TRANSFORM_CLI")
THREE_D_TIMEOUT_S = 2400
BG_REMOVAL_MODEL = "birefnet.safetensors"  # BiRefNet, MIT licence
TRANSPARENT_HINT = ", isolated on a plain solid white background, centered, full object visible, no shadow"


# ---------------------------------------------------------------------------
# ComfyUI client helpers
# ---------------------------------------------------------------------------
_http = httpx.AsyncClient(
    base_url=COMFY_URL,
    timeout=httpx.Timeout(60.0, connect=5.0),
    trust_env=not bool(os.environ.get("RENDER_BRIDGE_MANAGED_ID")),
)
_model_cache: dict[str, Any] = {"t": 0.0, "lists": {}}

# ---------------------------------------------------------------------------
# Lifecycle: track backend activity and optionally unload idle models
# ---------------------------------------------------------------------------
_state: dict[str, Any] = {
    "last_activity": time.time(),
    "models_loaded": True,  # unknown at startup; assume loaded so an idle unload happens once
    "last_history_id": None,
    "history_seen": False,
    "last_unload": None,
    "last_unload_reason": None,
}

# Background job tasks and full results live in memory. A private journal retains status and file references.
_jobs: dict[str, dict[str, Any]] = {}
_current_job: contextvars.ContextVar[dict[str, Any] | None] = contextvars.ContextVar("current_job", default=None)
JOB_RETENTION_S = 24 * 3600
JOB_HISTORY_LIMIT = 256
JOB_OUTPUT_LIMIT = 512
JOB_ACTIVE = {"queued", "running", "recovering", "cancel_requested", "cancel_unknown"}
JOB_PROMPT_ID = re.compile(r"[A-Za-z0-9_-]{1,128}")
# The project (name, context version) the current tool call works for; set by use_project().
_call_project: contextvars.ContextVar[tuple[str, int] | None] = contextvars.ContextVar("call_project", default=None)


async def comfy_up(timeout: float = 3) -> bool:
    try:
        return (await _http.get("/system_stats", timeout=timeout)).status_code == 200
    except httpx.HTTPError:
        return False


async def comfy_listening() -> bool:
    """Whether something takes connections on ComfyUI's port: true for a ComfyUI that is busy or hung, false for none."""
    url = urlsplit(COMFY_URL)
    try:
        port = url.port or (443 if url.scheme == "https" else 80)
        _, writer = await asyncio.wait_for(asyncio.open_connection(url.hostname, port), timeout=3)
    except (OSError, asyncio.TimeoutError):
        return False
    writer.close()
    return True


def paused_since() -> str | None:
    """When gaming mode was switched on, or None when the server is not paused."""
    try:
        return PAUSE_FLAG.read_text(encoding="utf-8-sig").strip() or "unknown"
    except FileNotFoundError:
        return None
    except OSError:
        return "unknown"


def _comfy_lost(error: Exception, prompt_id: str = "") -> ToolError:
    if PAUSE_FLAG.exists() and not prompt_id:
        return ToolError(PAUSED_MSG)
    prompt = f" while running prompt {prompt_id}" if prompt_id else ""
    return ToolError(f"Lost the connection to ComfyUI{prompt}: {error!r}")


async def ensure_comfy() -> None:
    """Wait for a reachable ComfyUI backend while gaming mode is off."""
    if PAUSE_FLAG.exists():
        raise ToolError(PAUSED_MSG)
    if await comfy_up():
        return
    if await comfy_listening():
        deadline = time.time() + COMFY_BUSY_WAIT_S
        while time.time() < deadline:
            if PAUSE_FLAG.exists():
                raise ToolError(PAUSED_MSG)
            if await comfy_up(timeout=10):
                return
            if not await comfy_listening():
                break
            await asyncio.sleep(2)
        else:
            raise ToolError(f"ComfyUI is running but has not answered for {COMFY_BUSY_WAIT_S:g} s: it is busy with a heavy "
                            "job, or it hung. Check the backend before retrying.")
        if PAUSE_FLAG.exists():
            raise ToolError(PAUSED_MSG)
    raise ToolError("ComfyUI is unavailable. Start it separately and check COMFY_URL.")


def mark_activity() -> None:
    _state["last_activity"] = time.time()
    _state["models_loaded"] = True


async def unload(reason: str) -> dict[str, Any]:
    r = await _http.post("/free", json={"unload_models": True, "free_memory": True}, timeout=10)
    r.raise_for_status()
    _state.update(models_loaded=False, last_unload=time.time(), last_unload_reason=reason)
    log(f"Unloaded models ({reason})")
    await asyncio.sleep(3)  # ComfyUI applies the flag on its worker loop
    return await gpu_info()


async def gpu_info() -> dict[str, Any]:
    try:
        dev = (await _http.get("/system_stats", timeout=5)).json().get("devices", [{}])[0]
        return {"gpu": dev.get("name"), "vram_total_gb": round(dev.get("vram_total", 0) / 2**30, 1),
                "vram_free_gb": round(dev.get("vram_free", 0) / 2**30, 1)}
    except Exception:  # noqa: BLE001
        return {}


async def idle_check() -> None:
    if PAUSE_FLAG.exists():
        return
    if not await comfy_up():
        _state["models_loaded"] = False
        return
    response = await _http.get("/queue", timeout=5)
    response.raise_for_status()
    queue = response.json()
    if not isinstance(queue, dict) or not all(
        isinstance(queue.get(key), list) for key in ("queue_running", "queue_pending")
    ):
        return
    if queue["queue_running"] or queue["queue_pending"]:
        mark_activity()
        return
    response = await _http.get("/history", params={"max_items": 1}, timeout=5)
    response.raise_for_status()
    history = response.json()
    if not isinstance(history, dict):
        return
    last_id = next(iter(history), None)
    if not _state["history_seen"] or last_id != _state["last_history_id"]:
        _state.update(history_seen=True, last_history_id=last_id)
        mark_activity()
        return
    idle = time.time() - _state["last_activity"]
    if IDLE_UNLOAD_S > 0 and _state["models_loaded"] and idle >= IDLE_UNLOAD_S:
        await unload(f"idle {idle / 60:.0f} min")


async def idle_watcher() -> None:
    """Track backend activity and optionally unload models when the queue is idle."""
    while True:
        await asyncio.sleep(15)
        try:
            _prune_jobs()
            await idle_check()
        except Exception as e:  # noqa: BLE001
            log(f"idle watcher: {e!r}")


def ensure_record_dir() -> None:
    PROVENANCE_DIR.mkdir(mode=0o700, parents=True, exist_ok=True)


def lifecycle_status() -> dict[str, Any]:
    idle = time.time() - _state["last_activity"]
    return {
        "models_loaded": _state["models_loaded"],
        "idle_seconds": round(idle),
        "idle_unload_after_seconds": round(IDLE_UNLOAD_S),
        "seconds_until_auto_unload": (max(0, round(IDLE_UNLOAD_S - idle)) if _state["models_loaded"] and IDLE_UNLOAD_S > 0 else None),
        "last_unload": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(_state["last_unload"])) if _state["last_unload"] else None,
        "last_unload_reason": _state["last_unload_reason"],
        "auto_start_comfy": False,
        "gaming_mode": (since := paused_since()) is not None,
        "gaming_mode_since": since,
    }


async def _loader_options(node: str, field: str) -> list[str]:
    r = await _http.get(f"/object_info/{node}")
    r.raise_for_status()
    spec = r.json()[node]["input"]["required"][field]
    if isinstance(spec[0], list):
        return list(spec[0])
    return list(spec[1].get("options", []))


async def model_lists(refresh: bool = False) -> dict[str, list[str]]:
    if not refresh and time.time() - _model_cache["t"] < 30:
        return _model_cache["lists"]
    # Check the backend first so an outage is not reported as missing model files.
    await ensure_comfy()
    pairs = {
        "diffusion_models": ("UNETLoader", "unet_name"),
        "checkpoints": ("CheckpointLoaderSimple", "ckpt_name"),
        "text_encoders": ("CLIPLoader", "clip_name"),
        "vae": ("VAELoader", "vae_name"),
        "loras": ("LoraLoaderModelOnly", "lora_name"),
        "background_removal": ("LoadBackgroundRemovalModel", "bg_removal_name"),
        "clip_vision": ("CLIPVisionLoader", "clip_name"),
    }
    out: dict[str, list[str]] = {}
    failed = False
    for key, (node, field) in pairs.items():
        try:
            out[key] = await _loader_options(node, field)
        except Exception as e:  # noqa: BLE001
            out[key] = [f"<error: {e}>"]
            failed = True
    if not failed:
        _model_cache.update(t=time.time(), lists=out)
    return out


def _resolve(name: str, options: list[str]) -> str | None:
    """Match a bare filename against ComfyUI's list (which may include subfolders like 'Flux\\ae.safetensors')."""
    if name in options:
        return name
    for opt in options:
        if opt.replace("\\", "/").split("/")[-1] == name:
            return opt
    return None


async def resolve_preset(preset: str) -> dict[str, Any]:
    if preset not in PRESETS:
        raise ToolError(f"Unknown preset '{preset}'. Choose one of: {', '.join(PRESETS)}")
    p = dict(PRESETS[preset])
    lists = await model_lists()
    missing = []
    keys = (("ckpt", "checkpoints"),) if p.get("ckpt") else (("unet", "diffusion_models"), ("clip", "text_encoders"), ("vae", "vae"))
    for key, list_key in keys:
        resolved = _resolve(p[key], lists[list_key])
        if resolved is None:
            missing.append(p[key])
        p[key] = resolved
    if p["lora"]:
        resolved = _resolve(p["lora"][0], lists["loras"])
        if resolved is None:
            missing.append(p["lora"][0])
        p["lora"] = (resolved, p["lora"][1])
    if missing:
        raise ToolError(f"Preset '{preset}' is missing model files on the server: {missing}")
    return p


def _lora_family(comfy_name: str) -> str | None:
    """The family folder a LoRA sits in (models/loras/<family>/<file>), or None for one at the top level."""
    parts = comfy_name.replace("\\", "/").split("/")
    return parts[0] if len(parts) > 1 else None


def _basename(comfy_name: str) -> str:
    return comfy_name.replace("\\", "/").split("/")[-1]


def _is_style_lora(comfy_name: str) -> bool:
    """A LoRA agents may pick: a .safetensors file in a family folder (files at the top of models/loras belong to
    presets, and pickle formats are never offered)."""
    return bool(_lora_family(comfy_name)) and comfy_name.lower().endswith(".safetensors") and not comfy_name.startswith("<error")


def lora_catalog(options: list[str]) -> dict[str, list[dict[str, Any]]]:
    """Style LoRAs agents may use, grouped by family."""
    out: dict[str, list[dict[str, Any]]] = {}
    for opt in options:
        if not _is_style_lora(opt):
            continue
        fam = _lora_family(opt)
        note = LORA_NOTES.get(_basename(opt), {})
        out.setdefault(fam, []).append({
            "name": _basename(opt), "about": note.get("about", "(no notes: added by hand)"),
            "trigger": note.get("trigger", ""), "suggested_strength": note.get("strength", 1.0)})
    return out


async def resolve_loras(preset: str, p: dict[str, Any], loras: list[Any] | None) -> list[dict[str, Any]]:
    """Check that the requested style LoRAs are installed and suit the preset's family.

    Returns [{name, file, strength, trigger}], where file is ComfyUI's name for it (with its family folder)."""
    if not loras:
        return []
    if not isinstance(loras, list) or len(loras) > MAX_LORAS:
        raise ToolError(f'loras is a list of up to {MAX_LORAS} items like {{"name": "<file name>", "strength": 1.0}}.')
    options = [o for o in (await model_lists())["loras"] if _is_style_lora(o)]
    family = p["family"]
    out = []
    for item in loras:
        if isinstance(item, str):
            item = {"name": item}
        if not isinstance(item, dict):
            raise ToolError(f'Each LoRA is {{"name": "<file name>", "strength": 1.0}}, not {item!r}.')
        name = str(item.get("name") or "").strip().replace("\\", "/")
        key = name.lower() if name.lower().endswith(".safetensors") else name.lower() + ".safetensors"
        matches = [o for o in options if o.replace("\\", "/").lower() == key or _basename(o).lower() == key]
        if not matches:
            raise ToolError(f"LoRA '{name}' is not installed. list_models shows the LoRAs for each family.")
        fit = [o for o in matches if _lora_family(o) == family]
        if not fit:
            fams = sorted({_lora_family(o) or "" for o in matches})
            usable = [n for n, q in PRESETS.items() if q["family"] in fams]
            raise ToolError(f"LoRA '{name}' is for the {', '.join(fams)} family, but preset '{preset}' is {family}. "
                            f"Use it with: {', '.join(usable) or 'no preset yet'}.")
        if any(o["file"] == fit[0] for o in out):
            raise ToolError(f"LoRA '{name}' is listed twice; give it one entry with the strength you want.")
        note = LORA_NOTES.get(_basename(fit[0]), {})
        raw = item.get("strength")
        if raw is None:
            strength = float(note.get("strength", 1.0))
        elif isinstance(raw, bool) or not isinstance(raw, (int, float, str)):
            raise ToolError(f"LoRA '{name}': strength must be a number.")
        else:
            try:
                strength = float(raw)
            except (ValueError, OverflowError):
                raise ToolError(f"LoRA '{name}': strength must be a number.") from None
        if not -2.0 <= strength <= 2.0:
            raise ToolError(f"LoRA '{name}': strength must be between -2 and 2 (usually 0.5-1.2).")
        out.append({"name": _basename(fit[0]), "file": fit[0], "strength": strength, "trigger": note.get("trigger", "")})
    return out


_lora_hashes: dict[str, tuple[float, int, str]] = {}


def _lora_sha256(comfy_name: str) -> str | None:
    """sha256 of an installed LoRA (cached by size and mtime), so the record shows exactly which file was applied."""
    path = COMFY_ROOT / "models" / "loras" / comfy_name.replace("\\", "/")
    try:
        st = path.stat()
        hit = _lora_hashes.get(str(path))
        if hit and hit[0] == st.st_mtime and hit[1] == st.st_size:
            return hit[2]
        h = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(8 << 20), b""):
                h.update(chunk)
    except OSError:
        return None
    _lora_hashes[str(path)] = (st.st_mtime, st.st_size, h.hexdigest())
    return h.hexdigest()


def _anima_tags(text: str) -> str:
    """Anima's recommended tags: quality tags unless the prompt has its own, and the "safe" safety tag unless the
    prompt sets one. At CFG 1 (anima_turbo) the negative prompt is skipped, so "safe" is the only content steering."""
    add = []
    if "masterpiece" not in text.lower() and "best quality" not in text.lower():
        add.append(ANIMA_QUALITY_TAGS)
    if not _ANIMA_SAFETY_TAG.search(text):
        add.append("safe")
    return ", ".join(add + [text])


def _apply_loras(g: dict[str, Any], p: dict[str, Any], model_ref: list[Any], clip_ref: list[Any]) -> tuple[list[Any], list[Any]]:
    """Chain the requested style LoRAs (p["user_loras"]) after the preset's model and text encoder."""
    for i, lora in enumerate(p.get("user_loras") or []):
        node = f"style_lora{i}"
        g[node] = {"class_type": "LoraLoader", "inputs": {
            "model": model_ref, "clip": clip_ref, "lora_name": lora["file"],
            "strength_model": lora["strength"], "strength_clip": lora["strength"]}}
        model_ref, clip_ref = [node, 0], [node, 1]
    return model_ref, clip_ref


def _snap(v: int, multiple: int = 16) -> int:
    return max(256, min(2048, int(round(v / float(multiple))) * multiple))


def build_t2i_graph(
    p: dict[str, Any],
    prompt: str,
    negative: str | None,
    width: int,
    height: int,
    seed: int,
    steps: int,
    cfg: float,
    batch_size: int,
    transparent: bool,
    prefix: str,
) -> dict[str, Any]:
    g: dict[str, Any] = {
        "unet": {"class_type": "UNETLoader", "inputs": {"unet_name": p["unet"], "weight_dtype": "default"}},
        "clip": {"class_type": "CLIPLoader", "inputs": {"clip_name": p["clip"], "type": p["clip_type"], "device": "default"}},
        "vae": {"class_type": "VAELoader", "inputs": {"vae_name": p["vae"]}},
    }
    model_ref: list[Any] = ["unet", 0]
    if p["lora"]:
        g["lora"] = {
            "class_type": "LoraLoaderModelOnly",
            "inputs": {"model": model_ref, "lora_name": p["lora"][0], "strength_model": p["lora"][1]},
        }
        model_ref = ["lora", 0]
    model_ref, clip_ref = _apply_loras(g, p, model_ref, ["clip", 0])
    g["shift"] = {"class_type": "ModelSamplingAuraFlow", "inputs": {"model": model_ref, "shift": p["shift"]}}
    g["pos"] = {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": prompt}}
    if negative and cfg != 1.0:
        g["neg"] = {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": negative}}
    else:
        g["neg"] = {"class_type": "ConditioningZeroOut", "inputs": {"conditioning": ["pos", 0]}}
    g["latent"] = {"class_type": "EmptySD3LatentImage", "inputs": {"width": width, "height": height, "batch_size": batch_size}}
    g["sampler"] = {
        "class_type": "KSampler",
        "inputs": {
            "model": ["shift", 0], "positive": ["pos", 0], "negative": ["neg", 0], "latent_image": ["latent", 0],
            "seed": seed, "steps": steps, "cfg": cfg, "sampler_name": p["sampler"], "scheduler": p["scheduler"],
            "denoise": 1.0,
        },
    }
    g["decode"] = {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["vae", 0]}}
    return _finish(g, transparent, prefix)


def _finish(g: dict[str, Any], transparent: bool, prefix: str) -> dict[str, Any]:
    """Add save nodes after g["decode"] (plus a BiRefNet cut-out when transparent)."""
    if transparent:
        _add_cutout(g, ["decode", 0], prefix)
        g["save_raw"] = {"class_type": "SaveImage", "inputs": {"images": ["decode", 0], "filename_prefix": prefix + "_raw"}}
    else:
        g["save"] = {"class_type": "SaveImage", "inputs": {"images": ["decode", 0], "filename_prefix": prefix}}
    return g


def build_neta_graph(p, prompt, negative, width, height, seed, steps, cfg, batch_size, transparent, prefix):
    """NetaYume Lumina (anime). Mirrors ComfyUI's image_netayume_lumina_t2i template."""
    g: dict[str, Any] = {"ckpt": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": p["ckpt"]}}}
    model_ref, clip_ref = _apply_loras(g, p, ["ckpt", 0], ["ckpt", 1])
    g.update({
        "shift": {"class_type": "ModelSamplingAuraFlow", "inputs": {"model": model_ref, "shift": p["shift"]}},
        "pos": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": NETA_POS_SYSTEM + prompt}},
        "neg": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": NETA_NEG_SYSTEM + (negative or NETA_NEG_DEFAULT)}},
        "latent": {"class_type": "EmptySD3LatentImage", "inputs": {"width": width, "height": height, "batch_size": batch_size}},
        "sampler": {"class_type": "KSampler", "inputs": {
            "model": ["shift", 0], "positive": ["pos", 0], "negative": ["neg", 0], "latent_image": ["latent", 0],
            "seed": seed, "steps": steps, "cfg": cfg, "sampler_name": p["sampler"], "scheduler": p["scheduler"], "denoise": 1.0}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["ckpt", 2]}},
    })
    return _finish(g, transparent, prefix)


def build_hidream_graph(p, prompt, negative, width, height, seed, steps, cfg, batch_size, transparent, prefix):
    """HiDream-O1 text-to-image. Mirrors ComfyUI's image_hidream_o1 template (prompt-refine branch omitted)."""
    g: dict[str, Any] = {"ckpt": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": p["ckpt"]}}}
    model_ref, clip_ref = _apply_loras(g, p, ["ckpt", 0], ["ckpt", 1])
    g.update({
        "noise_scale": {"class_type": "ModelNoiseScale", "inputs": {"model": model_ref, "noise_scale": p["noise_scale"]}},
        "seam": {"class_type": "HiDreamO1PatchSeamSmoothing", "inputs": {
            "model": ["noise_scale", 0], "start_percent": 0.8, "end_percent": 1.0, "pattern": "single_shift",
            "passes": "ramp_2_4", "blend": "median", "strength": 1.0}},
        "pos": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": prompt}},
        "neg": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": negative or ""}},
        "latent": {"class_type": "EmptyHiDreamO1LatentImage", "inputs": {"width": width, "height": height, "batch_size": batch_size}},
        "ksampler": {"class_type": "KSamplerSelect", "inputs": {"sampler_name": p["sampler"]}},
        "sigmas": {"class_type": "BasicScheduler", "inputs": {"model": ["noise_scale", 0], "scheduler": p["scheduler"], "steps": steps, "denoise": 1.0}},
        "sampler": {"class_type": "SamplerCustom", "inputs": {
            "model": ["seam", 0], "add_noise": True, "noise_seed": seed, "cfg": cfg, "positive": ["pos", 0],
            "negative": ["neg", 0], "sampler": ["ksampler", 0], "sigmas": ["sigmas", 0], "latent_image": ["latent", 0]}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["ckpt", 2]}},
    })
    return _finish(g, transparent, prefix)


def build_flux_graph(p, prompt, negative, width, height, seed, steps, cfg, batch_size, transparent, prefix):
    """FLUX.1-dev all-in-one checkpoint. Mirrors ComfyUI's flux templates (FluxGuidance 3.5, cfg 1, zeroed negative)."""
    g: dict[str, Any] = {"ckpt": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": p["ckpt"]}}}
    model_ref, clip_ref = _apply_loras(g, p, ["ckpt", 0], ["ckpt", 1])
    g.update({
        "text": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": prompt}},
        "pos": {"class_type": "FluxGuidance", "inputs": {"conditioning": ["text", 0], "guidance": p["guidance"]}},
        "neg": {"class_type": "ConditioningZeroOut", "inputs": {"conditioning": ["text", 0]}},
        "latent": {"class_type": "EmptySD3LatentImage", "inputs": {"width": width, "height": height, "batch_size": batch_size}},
        "sampler": {"class_type": "KSampler", "inputs": {
            "model": model_ref, "positive": ["pos", 0], "negative": ["neg", 0], "latent_image": ["latent", 0],
            "seed": seed, "steps": steps, "cfg": cfg, "sampler_name": p["sampler"], "scheduler": p["scheduler"], "denoise": 1.0}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["ckpt", 2]}},
    })
    return _finish(g, transparent, prefix)


def build_sd35_graph(p, prompt, negative, width, height, seed, steps, cfg, batch_size, transparent, prefix):
    """SD 3.5 Large all-in-one checkpoint. Mirrors ComfyUI's sd3.5_simple_example template."""
    g: dict[str, Any] = {"ckpt": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": p["ckpt"]}}}
    model_ref, clip_ref = _apply_loras(g, p, ["ckpt", 0], ["ckpt", 1])
    g.update({
        "pos": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": prompt}},
        "neg": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": negative or ""}},
        "latent": {"class_type": "EmptySD3LatentImage", "inputs": {"width": width, "height": height, "batch_size": batch_size}},
        "sampler": {"class_type": "KSampler", "inputs": {
            "model": model_ref, "positive": ["pos", 0], "negative": ["neg", 0], "latent_image": ["latent", 0],
            "seed": seed, "steps": steps, "cfg": cfg, "sampler_name": p["sampler"], "scheduler": p["scheduler"], "denoise": 1.0}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["ckpt", 2]}},
    })
    return _finish(g, transparent, prefix)


def build_sdxl_graph(p, prompt, negative, width, height, seed, steps, cfg, batch_size, transparent, prefix):
    """SDXL all-in-one checkpoint (Juggernaut XI). Mirrors ComfyUI's image_sdxl_simple template."""
    g: dict[str, Any] = {"ckpt": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": p["ckpt"]}}}
    model_ref, clip_ref = _apply_loras(g, p, ["ckpt", 0], ["ckpt", 1])
    g.update({
        "pos": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": prompt}},
        "neg": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": negative or ""}},
        "latent": {"class_type": "EmptyLatentImage", "inputs": {"width": width, "height": height, "batch_size": batch_size}},
        "sampler": {"class_type": "KSampler", "inputs": {
            "model": model_ref, "positive": ["pos", 0], "negative": ["neg", 0], "latent_image": ["latent", 0],
            "seed": seed, "steps": steps, "cfg": cfg, "sampler_name": p["sampler"], "scheduler": p["scheduler"], "denoise": 1.0}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["ckpt", 2]}},
    })
    return _finish(g, transparent, prefix)


def build_anima_graph(p, prompt, negative, width, height, seed, steps, cfg, batch_size, transparent, prefix):
    """Anima (2B, Cosmos-Predict2 based). Mirrors ComfyUI's image_anima_base_v1 template: no shift node, a plain
    EmptyLatentImage, and the Qwen3 0.6B text encoder loaded as type stable_diffusion."""
    g: dict[str, Any] = {
        "unet": {"class_type": "UNETLoader", "inputs": {"unet_name": p["unet"], "weight_dtype": "default"}},
        "clip": {"class_type": "CLIPLoader", "inputs": {"clip_name": p["clip"], "type": "stable_diffusion", "device": "default"}},
        "vae": {"class_type": "VAELoader", "inputs": {"vae_name": p["vae"]}},
    }
    model_ref, clip_ref = _apply_loras(g, p, ["unet", 0], ["clip", 0])
    g.update({
        "pos": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": prompt}},
        "neg": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": negative or ""}},
        "latent": {"class_type": "EmptyLatentImage", "inputs": {"width": width, "height": height, "batch_size": batch_size}},
        "sampler": {"class_type": "KSampler", "inputs": {
            "model": model_ref, "positive": ["pos", 0], "negative": ["neg", 0], "latent_image": ["latent", 0],
            "seed": seed, "steps": steps, "cfg": cfg, "sampler_name": p["sampler"], "scheduler": p["scheduler"], "denoise": 1.0}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["vae", 0]}},
    })
    return _finish(g, transparent, prefix)


def build_flux3_graph(p, prompt, negative, width, height, seed, steps, cfg, batch_size, transparent, prefix):
    """FLUX.3 Image Klein (distilled or base). Mirrors ComfyUI's image_flux3_klein_t2i and _distilled_t2i templates,
    minus their optional prompt-upsampler branch: agents already write detailed prompts."""
    g: dict[str, Any] = {
        "unet": {"class_type": "UNETLoader", "inputs": {"unet_name": p["unet"], "weight_dtype": "default"}},
        "clip": {"class_type": "CLIPLoader", "inputs": {"clip_name": p["clip"], "type": "flux3", "device": "default"}},
        "vae": {"class_type": "VAELoader", "inputs": {"vae_name": p["vae"]}},
    }
    model_ref, clip_ref = _apply_loras(g, p, ["unet", 0], ["clip", 0])
    g.update({
        "pos": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": prompt}},
        "neg": {"class_type": "CLIPTextEncode", "inputs": {"clip": clip_ref, "text": negative or ""}},
        "latent": {"class_type": "EmptyFlux2LatentImage", "inputs": {"width": width, "height": height, "batch_size": batch_size}},
        "sampler": {"class_type": "KSampler", "inputs": {
            "model": model_ref, "positive": ["pos", 0], "negative": ["neg", 0], "latent_image": ["latent", 0],
            "seed": seed, "steps": steps, "cfg": cfg, "sampler_name": p["sampler"], "scheduler": p["scheduler"], "denoise": 1.0}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["vae", 0]}},
    })
    return _finish(g, transparent, prefix)


def build_graph(p, *args) -> dict[str, Any]:
    builder = {"neta": build_neta_graph, "hidream": build_hidream_graph, "flux": build_flux_graph,
               "sd35": build_sd35_graph, "sdxl": build_sdxl_graph, "anima": build_anima_graph,
               "flux3": build_flux3_graph}.get(p.get("builder", ""), build_t2i_graph)
    return builder(p, *args)


def _add_cutout(g: dict[str, Any], image_ref: list[Any], prefix: str) -> None:
    g["bg_model"] = {"class_type": "LoadBackgroundRemovalModel", "inputs": {"bg_removal_name": BG_REMOVAL_MODEL}}
    g["bg_mask"] = {"class_type": "RemoveBackground", "inputs": {"bg_removal_model": ["bg_model", 0], "image": image_ref}}
    # RemoveBackground returns a foreground mask (1 = keep); JoinImageWithAlpha expects ComfyUI's inverted convention.
    g["bg_inv"] = {"class_type": "InvertMask", "inputs": {"mask": ["bg_mask", 0]}}
    g["rgba"] = {"class_type": "JoinImageWithAlpha", "inputs": {"image": image_ref, "alpha": ["bg_inv", 0]}}
    g["save"] = {"class_type": "SaveImage", "inputs": {"images": ["rgba", 0], "filename_prefix": prefix}}


async def queue_and_wait(graph: dict[str, Any], timeout_s: int = DEFAULT_TIMEOUT_S) -> tuple[str, dict[str, Any], float]:
    await ensure_comfy()
    mark_activity()
    try:
        return await _queue_and_wait(graph, timeout_s)
    finally:
        mark_activity()  # idle timer starts when the job ends


async def _queue_and_wait(graph: dict[str, Any], timeout_s: int) -> tuple[str, dict[str, Any], float]:
    if PAUSE_FLAG.exists():
        raise ToolError(PAUSED_MSG)
    t0 = time.time()
    job = _current_job.get()
    if job is not None:
        if job["status"] != "running":
            raise asyncio.CancelledError
        job["submission_pending"] = True
        if not _remember_job(job):
            job["submission_pending"] = False
            raise ToolError("Could not record the background job before submitting to ComfyUI.")
    try:
        r = await _http.post("/prompt", json={"prompt": graph, "client_id": f"mcp-{uuid.uuid4().hex[:8]}"})
    except httpx.HTTPError as e:
        raise _comfy_lost(e) from None
    if r.status_code != 200:
        if job is not None and 400 <= r.status_code < 500:
            job["submission_pending"] = False
            _remember_job(job)
        raise ToolError(f"ComfyUI rejected the workflow ({r.status_code}): {r.text[:2000]}")
    prompt_id = r.json()["prompt_id"]
    if not isinstance(prompt_id, str) or not JOB_PROMPT_ID.fullmatch(prompt_id):
        raise ToolError("ComfyUI did not return a usable prompt ID. Its submission may still be active.")
    _last_prompt.set(prompt_id)
    if job is not None:
        job["prompt_ids"].append(prompt_id)
        job["submission_pending"] = False
        _remember_job(job)
    started: float | None = None  # when ComfyUI began running it: the run timeout counts from here
    next_check, missing = 0.0, 0
    try:
        while True:
            now = time.time()
            if started is None and now >= next_check:
                next_check = now + 2
                state = await _prompt_state(prompt_id)
                if state == "running":
                    started = now
                missing = missing + 1 if state == "gone" else 0
                if missing >= 2:
                    raise ToolError(f"Prompt {prompt_id} was removed from ComfyUI's queue before it ran (cancelled "
                                    "from another session or the ComfyUI web page). Nothing was generated.")
            if started is not None and now - started > timeout_s:
                raise ToolError(f"Timed out after {timeout_s}s of running prompt {prompt_id} (it may still be running).")
            if started is None and now - t0 > QUEUE_WAIT_MAX_S:
                removed = await _dequeue(prompt_id)
                queue = await _upstream_queue() if removed else None
                outcome = ("The prompt is no longer in the queue."
                           if queue is not None and prompt_id not in queue
                           else "Its removal could not be confirmed. It may still be queued or running.")
                raise ToolError(f"Gave up after {QUEUE_WAIT_MAX_S / 60:g} min waiting in ComfyUI's queue behind other "
                                f"jobs. {outcome}")
            try:
                h = await _http.get(f"/history/{prompt_id}")
            except httpx.HTTPError as e:
                if not await comfy_up():
                    raise _comfy_lost(e, prompt_id) from None
                await asyncio.sleep(2)
                continue
            if h.status_code == 200 and prompt_id in h.json():
                entry = h.json()[prompt_id]
                status = entry.get("status", {})
                if status.get("status_str") == "error":
                    msgs = [m for m in status.get("messages", []) if m and m[0] == "execution_error"]
                    detail = msgs[0][1] if msgs else status
                    raise ToolError(f"ComfyUI execution error: {json.dumps(detail)[:2000]}")
                if status.get("completed", True):
                    return prompt_id, entry, time.time() - (started or t0)
            await asyncio.sleep(0.5)
    except asyncio.CancelledError:
        if started is None and job is None:
            await _dequeue(prompt_id)
        raise


async def _prompt_state(prompt_id: str) -> str:
    """'waiting' (still in the queue), 'running' (running or already finished), 'gone' (removed), or 'unknown'."""
    try:
        r = await _http.get(f"/api/jobs/{prompt_id}", timeout=5)
        if r.status_code == 404 and "Job not found" in r.text:  # (a ComfyUI without /api/jobs gives a plain 404)
            return "gone"
        if r.status_code != 200:
            return "unknown"
        return "waiting" if r.json().get("status") == "pending" else "running"
    except Exception:  # noqa: BLE001
        return "unknown"


async def _dequeue(*prompt_ids: str) -> bool:
    try:
        response = await _http.post("/queue", json={"delete": list(prompt_ids)}, timeout=5)
        return response.is_success
    except httpx.HTTPError:
        return False


def comfy_view_url(img: dict[str, Any]) -> str:
    """Internal ComfyUI /view URL (localhost only)."""
    q = urlencode({"filename": img["filename"], "subfolder": img.get("subfolder", ""), "type": img.get("type", "output")})
    return f"{COMFY_URL}/view?{q}"


def signed_url(img: dict[str, Any], base: str = PUBLIC_URL) -> str:
    """Public download URL served by this MCP server: /files/<name>?subfolder&type&exp&sig (no auth header needed)."""
    filename, subfolder, ftype = img["filename"], img.get("subfolder", ""), img.get("type", "output")
    exp = int(time.time() + LINK_TTL_S)
    q = urlencode({"subfolder": subfolder, "type": ftype, "exp": exp, "sig": _link_sig(filename, subfolder, ftype, exp)})
    return f"{base}/files/{quote(filename)}?{q}"


async def collect_images(entry: dict[str, Any], primary_node: str | None = None) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for node_id, node_out in entry.get("outputs", {}).items():
        files = [
            (kind, item)
            for kind, items in node_out.items() if isinstance(items, list)
            for item in items if isinstance(item, dict) and "filename" in item
        ]
        for kind, img in files:
            if img.get("type") == "temp":
                continue
            out.append({
                "node": node_id,
                "kind": kind,
                "role": "primary" if (primary_node is None or node_id == primary_node) else "secondary",
                "filename": img["filename"],
                "subfolder": img.get("subfolder", ""),
                "type": img.get("type", "output"),
                "url": signed_url(img),
                "urls": {name: signed_url(img, base) for name, base in PUBLIC_URLS.items()},
            })
    out.sort(key=lambda i: i["role"] != "primary")
    return out


async def preview_content(img: dict[str, Any], max_side: int = 512) -> ImageContent | None:
    try:
        r = await _http.get(comfy_view_url(img))
        r.raise_for_status()
        im = PILImage.open(io.BytesIO(r.content))
        img["width"], img["height"], img["mode"] = im.width, im.height, im.mode
        im.thumbnail((max_side, max_side))
        buf = io.BytesIO()
        im.save(buf, format="PNG", optimize=True)
        return ImageContent(type="image", data=base64.b64encode(buf.getvalue()).decode(), mime_type="image/png")
    except Exception:  # noqa: BLE001
        return None


# ---------------------------------------------------------------------------
# Provenance: an append-only record of how each asset was authored
# ---------------------------------------------------------------------------
def _now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%S%z")


def _active_project() -> dict[str, Any] | None:
    try:
        return json.loads((PROVENANCE_DIR / "active_project.json").read_text(encoding="utf-8"))
    except Exception:  # noqa: BLE001
        return None


def _project_doc(name: str) -> dict[str, Any] | None:
    try:
        return json.loads((PROVENANCE_DIR / "projects" / f"{name}.json").read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None


def use_project(project: str = "") -> tuple[str | None, dict[str, Any] | None]:
    """Resolve the project a tool call works for and remember it for the authorship log.

    An explicit name wins over the server-wide active project, so agents working on different games at the same
    time never pick up each other's style. Returns (name, latest context version), or (None, None) when no
    project applies. An unknown explicit name is an error, so a typo can't silently drop a game's style.
    Every asset tool calls this first, so a missing record folder stops the call before any work is done."""
    ensure_record_dir()
    if project:
        name = _safe_name(project, "")
        doc = _project_doc(name) if name else None
        if not doc or not doc.get("versions"):
            raise ToolError(f"Unknown project {project!r}: call set_project_context(project=...) first.")
    else:
        name = (_active_project() or {}).get("project")
        doc = _project_doc(name) if name else None
        if not doc or not doc.get("versions"):
            _call_project.set(None)
            return None, None
    ctx = doc["versions"][-1]
    _call_project.set((name, ctx["version"]))
    return name, ctx


def _log_project() -> tuple[str | None, int | None]:
    cp = _call_project.get()
    if cp:
        return cp
    active = _active_project() or {}
    return active.get("project"), active.get("version")


def _rel_path(f: dict[str, Any]) -> str:
    return f"{f.get('type', 'output')}/{f.get('subfolder', '')}/{f['filename']}".replace("\\", "/").replace("//", "/")


def _file_sha256(f: dict[str, Any]) -> str | None:
    base = COMFY_DIRS.get(f.get("type", "output"))
    if base is None:
        return None
    try:
        return hashlib.sha256((base / f.get("subfolder", "") / f["filename"]).read_bytes()).hexdigest()
    except OSError:
        return None


def log_asset(tool: str, summary: dict[str, Any], files: list[dict[str, Any]]) -> tuple[str | None, int | None]:
    """Append one JSON line per tool result to provenance/assets.jsonl (never raises).

    Also stamps the project and context version into `summary`, so the tool result shows what it was logged under.
    Returns (project, context version)."""
    project, version = _log_project()
    summary["project"], summary["project_context_version"] = project, version
    job = _current_job.get()
    if job is not None:
        _remember_job_outputs(job, files)
    try:
        ensure_record_dir()
        skip = {"images", "files", "download_hint", "godot_hint", "preview", "files_in_zip", "project",
                "project_context_version"}
        rec = {
            "time": _now_iso(),
            "tool": tool,
            "project": project,
            "project_context_version": version,
            "job": {"id": job["id"], "label": job["label"]} if job else None,
            "details": {k: v for k, v in summary.items() if k not in skip},
            "outputs": [_rel_path(f) for f in files],
            "sha256": {_rel_path(f): _file_sha256(f) for f in files},
        }
        with open(PROVENANCE_DIR / "assets.jsonl", "a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
    except Exception as e:  # noqa: BLE001
        log(f"provenance log failed: {e!r}")
    return project, version


async def build_result(summary: dict[str, Any], images: list[dict[str, Any]], preview: bool, max_previews: int = 4,
                       tool: str = "") -> CallToolResult:
    import sys
    if images:
        log_asset(tool or sys._getframe(1).f_code.co_name, summary, images)
    content: list[Any] = []
    previews = []
    if preview:
        for img in [i for i in images if i["role"] == "primary"][:max_previews]:
            pc = await preview_content(img)
            if pc:
                previews.append(pc)
    summary["images"] = images
    summary["download_hint"] = (
        'curl -o <file> "<url>"  (images are full-resolution PNGs, transparent ones RGBA; 3D models are .glb). '
        "'urls' has one link per configured transfer address."
    )
    content.append(TextContent(type="text", text=json.dumps(summary, indent=2, ensure_ascii=False)))
    content.extend(previews)
    return CallToolResult(content=content)


# ---------------------------------------------------------------------------
# MCP server + tools
# ---------------------------------------------------------------------------
mcp = MCPServer(
    name="comfyui",
    title="Render Bridge",
    instructions=(
        "Provides tools for a locally managed ComfyUI instance. "
        "Use generate_image for game art; set transparent=true for sprites/icons/UI elements (returns RGBA PNG). "
        "Results contain full-size download URLs; fetch them with curl to save files locally. "
        "Presets: z_image_turbo (fast default), qwen_image (best quality/text), qwen_image_fast (4-step Qwen), "
        "anime (anime/illustration concept art), cinematic (Unreal-style cinematic renders), flux_dev "
        "(general/photoreal/painterly), sd35 (wide style range), anima_turbo / anima_aesthetic (Anima: anime, "
        "illustration and painterly art, fast / best-looking), juggernaut_xl (Juggernaut XI SDXL workhorse), "
        "flux3_klein / flux3_klein_base (FLUX.3 Image Klein, the newest BFL model: fast distilled / slower base). "
        "Switch models with generate_image(preset=...), or per game with set_project_context(default_preset=...). "
        "Style LoRAs (painterly, watercolor, gouache, oil, hand-painted textures) go in generate_image(loras=[{name, "
        "strength}]); each works only with presets of its family (sdxl: juggernaut_xl, flux1: flux_dev, anima: "
        "anima_*), and list_models lists them by family with their trigger words. "
        "edit_image edits an existing image or combines it with up to 2 references (same character in new poses, "
        "recolours, turnarounds). "
        "repaint_frames repaints a whole action's pose-guide frames (a rig's exported frames) in one call, with the "
        "same instruction, references and seed for every frame: frames in, finished frames out, in order. "
        "animate_from_clip repaints a character along a short driving clip (a rig's exported action, as a video or as "
        "frames): the character's image and the clip in, one frame per clip frame and an mp4 out (Wan-Animate-2). "
        "animate_loop makes a looping animation from ONE image: a short video that starts and ends on it, cut into "
        "evenly spaced frames, with an mp4 (Wan 2.2 image-to-video; the model invents the motion from the prompt). "
        "edit_region changes ONE AREA of an image (a rectangle or a mask) and keeps every other pixel exactly, at any "
        "image size: use it for local fixes to a large or approved image instead of building masked workflows by hand. "
        "image_to_3d turns an image (ideally a transparent generate_image result) into a textured GLB model. "
        "For slow work (image_to_3d, quality renders, batches) use start_job + get_job so calls don't time out. "
        "Finishing tools: optimize_glb (game-ready/mobile GLB), pack_spritesheet (sheet PNG + Godot SpriteFrames .tres), "
        "export_icons (iOS/Android/Godot/web/Windows icon sets). "
        "Model weights are installed separately and subject to their own licenses. "
        "Call set_project_context at the start of work on a game (art direction, theme, lore, and optionally a style "
        "lock: style_prompt, style_negative, default_preset, style_anchor); every asset is then logged with that "
        "context, its prompts, inputs and edits (asset_history) as an authorship record. Pass project=<game> on every "
        "asset call: it applies that game's style lock and logs under it (the server-wide active project is only a "
        "fallback). edit_image(match_style=true) uses the game's style anchor image. When the owner says which "
        "results they pick or reject, record it with approve_asset (their words in notes); never on your own judgment. "
        "ComfyUI loads models as needed. Call load_models to prepare a preset before a batch, unload_models to free "
        "the GPU immediately, server_status to see what is loaded. "
        "All sessions share one GPU queue, first come first served; time spent waiting in it doesn't count toward a "
        "job's timeout. When server_status shows a long queue, use start_job + get_job (which shows your place in line). "
        "To send a file to the server, call "
        "upload_link and run the curl it gives you; then pass the returned filename/subfolder with source=\"input\". "
        "When gaming mode is on, wait for the operator to turn it off before submitting GPU work."
    ),
)


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True), structured_output=False)
async def list_models() -> str:
    """List generation presets (with availability and model family), the style LoRAs for each family, and every
    model file ComfyUI can load."""
    await ensure_comfy()
    lists = await model_lists(refresh=True)
    presets = {}
    for name, p in PRESETS.items():
        try:
            await resolve_preset(name)
            ok, err = True, None
        except Exception as e:  # noqa: BLE001
            ok, err = False, str(e)
        presets[name] = {
            "available": ok, "error": err, "family": p["family"], "description": p["description"],
            "default_size": list(p["size"]), "default_steps": p["steps"], "default_cfg": p["cfg"],
        }
    loras = lora_catalog(lists.get("loras", []))
    for fam, items in loras.items():
        for item in items:
            item["presets"] = [n for n, q in PRESETS.items() if q["family"] == fam]
    stats = (await _http.get("/system_stats")).json()
    dev = stats.get("devices", [{}])[0]
    return json.dumps({
        "presets": presets,
        "loras": loras,
        "loras_how": 'generate_image(preset=<one of the LoRA\'s presets>, loras=[{"name": "<name>", "strength": 0.9}]). '
                     f"Up to {MAX_LORAS} at once; trigger words are added to the prompt for you.",
        "models": lists,
        "gpu": {"name": dev.get("name"), "vram_total_gb": round(dev.get("vram_total", 0) / 2**30, 1),
                "vram_free_gb": round(dev.get("vram_free", 0) / 2**30, 1)},
        "comfyui_version": stats.get("system", {}).get("comfyui_version"),
        "download_bases": PUBLIC_URLS,
        "image_to_3d_configured": bool(THREE_D_WORKFLOW),
    }, indent=2)


@mcp.tool(structured_output=False)
async def generate_image(
    prompt: str,
    preset: str = "",
    negative_prompt: str = "",
    width: int = 0,
    height: int = 0,
    seed: int = -1,
    steps: int = 0,
    cfg: float = 0.0,
    batch_size: int = 1,
    transparent: bool = False,
    add_background_hint: bool = True,
    filename_prefix: str = "asset",
    preview: bool = True,
    project: str = "",
    use_project_style: bool = True,
    loras: list[dict[str, Any] | str] | None = None,
) -> CallToolResult:
    """Generate image(s) from a text prompt.

    Args:
        prompt: What to draw (the subject). With a project style lock, the game's style_prompt is appended for you;
            without one, be specific about style (e.g. "2D game sprite, hand-painted, top-down view").
        preset: z_image_turbo (fast) | qwen_image (best quality & in-image text, ~50 steps) |
            qwen_image_fast (4-step Qwen) | anime (NetaYume Lumina: anime/illustration concept art) |
            cinematic (HiDream-O1: Unreal-Engine-style cinematic renders, environments, hero shots) |
            flux_dev (FLUX.1-dev: general/photoreal/painterly; takes FLUX.1 LoRAs) |
            sd35 (SD 3.5 Large: wide style range) |
            anima_turbo (Anima, fast: anime/illustration/painterly, not realism) |
            anima_aesthetic (Anima, slower, best-looking illustration) |
            juggernaut_xl (Juggernaut XI SDXL: versatile, pair with the painterly SDXL LoRAs) |
            flux3_klein (FLUX.3 Image Klein: newest BFL model, strong prompts, text and art styles, 16 steps) |
            flux3_klein_base (FLUX.3 Klein Base: more varied, real negative prompts, 50 steps).
            "" = the project's default_preset, else z_image_turbo. list_models shows every preset and its family.
        negative_prompt: Things to avoid (used by qwen_image, anime, cinematic, sd35, anima_aesthetic,
            juggernaut_xl, flux3_klein_base; anime, anima_aesthetic and juggernaut_xl have good defaults). The
            project's style_negative is added. Presets that run at CFG 1 (z_image_turbo, qwen_image_fast, flux_dev,
            anima_turbo, flux3_klein) ignore negatives; the result then shows negative_used null and keeps your
            text in negative_ignored.
        width, height: Output size in px (256-2048; multiples of 16, or 32 for cinematic). 0 = preset default
            (1024x1024 z_image_turbo/anime, 1328x1328 qwen, 1536x1536 cinematic). Qwen sizes: 1664x928 (16:9),
            928x1664 (9:16), 1472x1104 (4:3). Cinematic supports up to 2048x2048 (slower). Anima works from 512
            to 1536 px; SDXL is best near 1 megapixel (1024x1024, 832x1216, 1216x832).
        seed: -1 for random; reuse a returned seed to reproduce or iterate.
        steps, cfg: 0 = preset default.
        batch_size: Number of variations (1-8).
        transparent: Remove the background with BiRefNet and save an RGBA PNG (sprites, icons, UI). The un-cut
            original is also saved (role "secondary").
        add_background_hint: When transparent, append "isolated on a plain solid white background..." to the prompt
            so the cut-out is clean. Set false if your prompt already specifies the background.
        filename_prefix: Name stem for saved files (saved under ComfyUI/output/mcp/).
        preview: Include small inline previews in the result.
        project: the game this asset is for (see set_project_context). Applies its style lock and logs the asset
            under it. Default: the server-wide active project. Always pass it when several games are in progress.
        use_project_style: false = ignore the project's style lock for this call (still logged under the project).
        loras: style LoRAs to apply, up to 4: [{"name": "<file name>", "strength": 0.9}, ...] (a bare name, with or
            without .safetensors, also works). A LoRA only works with presets of its family (list_models lists them
            by family, e.g. sdxl -> juggernaut_xl, flux1 -> flux_dev, anima -> anima_turbo / anima_aesthetic).
            strength defaults to the LoRA's suggested value; its trigger words are added to the prompt when
            missing. Each LoRA's file and sha256 are recorded in the asset's history.

    Returns JSON with seed, timings, the exact prompt used and image URLs (download with curl), plus previews.
    """
    _, ctx = use_project(project)
    style = ((ctx or {}).get("style") or {}) if use_project_style else {}
    applied = [k for k in ("prompt", "negative") if style.get(k)]
    if not preset and style.get("preset"):
        applied.append("preset")
    preset = preset or style.get("preset") or "z_image_turbo"
    p = await resolve_preset(preset)
    p["user_loras"] = await resolve_loras(preset, p, loras)
    w = _snap(width or p["size"][0], p.get("snap", 16))
    h = _snap(height or p["size"][1], p.get("snap", 16))
    seed = random.randint(0, 2**50) if seed is None or seed < 0 else seed
    steps = steps or p["steps"]
    cfg = cfg or p["cfg"]
    batch_size = max(1, min(8, batch_size))
    safe_prefix = "".join(c for c in filename_prefix if c.isalnum() or c in "-_") or "asset"
    styled = f"{prompt}, {style['prompt']}" if style.get("prompt") else prompt
    # A LoRA's trigger words only work in the prompt; add the ones the prompt doesn't already contain.
    triggers = [lo["trigger"] for lo in p["user_loras"] if lo["trigger"] and lo["trigger"].lower() not in styled.lower()]
    if triggers:  # a trigger ending in "of" ("...texture of") goes last and runs straight into the subject
        triggers.sort(key=lambda t: t.endswith(" of"))
        styled = "".join(t + (" " if t.endswith(" of") else ", ") for t in triggers) + styled
    if p["family"] == "anima":
        styled = _anima_tags(styled)
    full_prompt = styled + (TRANSPARENT_HINT if transparent and add_background_hint else "")
    negative = negative_prompt or p["negative"]
    if style.get("negative"):
        negative = f"{negative}, {style['negative']}" if negative else style["negative"]
    negative_ignored = None
    if abs(cfg - 1.0) < 1e-6 and negative:
        # ComfyUI skips the negative pass at CFG 1 (the distilled presets), so record that none was used, and keep
        # what the caller or the project style asked for in the record.
        if negative_prompt or style.get("negative"):
            negative_ignored = negative
        negative = None
    graph = build_graph(p, full_prompt, negative, w, h, seed, steps, cfg, batch_size, transparent, f"mcp/{safe_prefix}")
    prompt_id, entry, elapsed = await queue_and_wait(graph)
    images = await collect_images(entry, primary_node="save")
    summary = {
        "prompt_id": prompt_id, "preset": preset, "seed": seed, "width": w, "height": h, "steps": steps, "cfg": cfg,
        "transparent": transparent, "prompt": prompt, "prompt_used": full_prompt, "negative_used": negative,
        "project_style_applied": applied, "seconds": round(elapsed, 1),
    }
    if negative_ignored:
        summary["negative_ignored"] = negative_ignored
        summary["note"] = f"negative prompts have no effect at CFG 1 ({preset}); raise cfg or use a preset that takes them."
    if p["user_loras"]:
        hashes = await asyncio.gather(*(asyncio.to_thread(_lora_sha256, lo["file"]) for lo in p["user_loras"]))
        summary["loras"] = [{"name": lo["name"], "file": lo["file"].replace("\\", "/"), "strength": lo["strength"],
                             "sha256": sha} for lo, sha in zip(p["user_loras"], hashes)]
        summary["lora_triggers_added"] = triggers
    return await build_result(summary, images, preview)


async def input_image_ref(filename: str, subfolder: str, source: str, image_base64: str) -> str:
    """Return a LoadImage reference ('sub/name.png [output]'), uploading base64 data first if given."""
    if image_base64:
        await ensure_comfy()
        raw = base64.b64decode(image_base64.split(",", 1)[-1])
        up_name = f"mcp_upload_{uuid.uuid4().hex[:10]}.png"
        r = await _http.post("/upload/image", files={"image": (up_name, raw, "image/png")}, data={"overwrite": "true"})
        r.raise_for_status()
        info = r.json()
        filename, subfolder, source = info["name"], info.get("subfolder", ""), info.get("type", "input")
    if not filename:
        raise ToolError("Provide filename (+subfolder/source) or image_base64.")
    if source not in ("output", "input"):
        raise ToolError("source must be 'output' or 'input'")
    rel = f"{subfolder}/{filename}" if subfolder else filename
    return f"{rel} [{source}]"


@mcp.tool(structured_output=False)
async def remove_background(
    filename: str = "",
    subfolder: str = "",
    source: str = "output",
    image_base64: str = "",
    filename_prefix: str = "cutout",
    preview: bool = True,
    project: str = "",
) -> CallToolResult:
    """Cut out the subject of an image and save it as a transparent RGBA PNG (BiRefNet).

    Either reference an image already on the server (filename/subfolder from a previous result, source "output"
    or "input"), or pass image_base64 (PNG/JPEG bytes, base64) to upload a small local image. For anything bigger,
    send the file with upload_link first and pass the filename/subfolder it returns with source="input".
    project: the game to log it under (default: active project).
    """
    use_project(project)
    annotated = await input_image_ref(filename, subfolder, source, image_base64)
    safe_prefix = "".join(c for c in filename_prefix if c.isalnum() or c in "-_") or "cutout"
    g: dict[str, Any] = {"load": {"class_type": "LoadImage", "inputs": {"image": annotated}}}
    _add_cutout(g, ["load", 0], f"mcp/{safe_prefix}")
    prompt_id, entry, elapsed = await queue_and_wait(g)
    images = await collect_images(entry, primary_node="save")
    return await build_result({"prompt_id": prompt_id, "source_image": annotated, "seconds": round(elapsed, 1)}, images, preview)


async def edit_models() -> dict[str, str]:
    """Resolve the Qwen-Image-Edit model files installed in ComfyUI."""
    await ensure_comfy()
    lists = await model_lists()
    m = {k: _resolve(v, lists["loras" if k == "lora" else {"unet": "diffusion_models", "clip": "text_encoders", "vae": "vae"}[k]])
         for k, v in EDIT_MODELS.items()}
    missing = [EDIT_MODELS[k] for k, v in m.items() if v is None]
    if missing:
        raise ToolError(f"edit_image is missing model files on the server: {missing}")
    return m


def build_edit_graph(m: dict[str, str], main: str, ref_names: list[str], prompt: str, negative_prompt: str, seed: int,
                     fast: bool, transparent: bool, prefix: str) -> dict[str, Any]:
    """Qwen-Image-Edit-2511: `main` is image 1, edited by the instruction; `ref_names` are image 2 and 3."""
    g: dict[str, Any] = {
        "unet": {"class_type": "UNETLoader", "inputs": {"unet_name": m["unet"], "weight_dtype": "default"}},
        "clip": {"class_type": "CLIPLoader", "inputs": {"clip_name": m["clip"], "type": "qwen_image", "device": "default"}},
        "vae": {"class_type": "VAELoader", "inputs": {"vae_name": m["vae"]}},
        "img1": {"class_type": "LoadImage", "inputs": {"image": main}},
        "scale": {"class_type": "FluxKontextImageScale", "inputs": {"image": ["img1", 0]}},
        "shift": {"class_type": "ModelSamplingAuraFlow", "inputs": {"model": ["unet", 0], "shift": 3.1}},
        "cfgnorm": {"class_type": "CFGNorm", "inputs": {"model": ["shift", 0], "strength": 1.0, "pre_cfg": False}},
    }
    model_ref: list[Any] = ["cfgnorm", 0]
    if fast:
        g["lora"] = {"class_type": "LoraLoaderModelOnly", "inputs": {"model": model_ref, "lora_name": m["lora"], "strength_model": 1.0}}
        model_ref = ["lora", 0]
    enc: dict[str, Any] = {"clip": ["clip", 0], "vae": ["vae", 0], "image1": ["scale", 0]}
    for i, name in enumerate(ref_names, start=2):
        g[f"img{i}"] = {"class_type": "LoadImage", "inputs": {"image": name}}
        enc[f"image{i}"] = [f"img{i}", 0]
    g["pos_enc"] = {"class_type": "TextEncodeQwenImageEditPlus", "inputs": {**enc, "prompt": prompt}}
    g["neg_enc"] = {"class_type": "TextEncodeQwenImageEditPlus", "inputs": {**enc, "prompt": "" if fast else negative_prompt}}
    for k in ("pos", "neg"):
        g[k] = {"class_type": "FluxKontextMultiReferenceLatentMethod",
                "inputs": {"conditioning": [f"{k}_enc", 0], "reference_latents_method": "index_timestep_zero"}}
    g["latent"] = {"class_type": "VAEEncode", "inputs": {"pixels": ["scale", 0], "vae": ["vae", 0]}}
    g["sampler"] = {"class_type": "KSampler", "inputs": {
        "model": model_ref, "positive": ["pos", 0], "negative": ["neg", 0], "latent_image": ["latent", 0], "seed": seed,
        "steps": 4 if fast else 40, "cfg": 1.0 if fast else 4.0, "sampler_name": "euler", "scheduler": "simple", "denoise": 1.0}}
    g["decode"] = {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["vae", 0]}}
    return _finish(g, transparent, prefix)


@mcp.tool(structured_output=False)
async def edit_image(
    prompt: str,
    filename: str = "",
    subfolder: str = "",
    source: str = "output",
    image_base64: str = "",
    reference_images: list[dict[str, str]] | None = None,
    fast: bool = True,
    seed: int = -1,
    negative_prompt: str = "",
    transparent: bool = False,
    filename_prefix: str = "edit",
    preview: bool = True,
    project: str = "",
    match_style: bool = False,
) -> CallToolResult:
    """Edit an image with an instruction, optionally using up to 2 reference images (Qwen-Image-Edit-2511, Apache-2.0).

    Good for: variations and recolours ("make the armour gold"); the SAME character in a new pose, expression,
    outfit or angle (turnarounds: "the same character seen from the back, full body"); adding/removing objects;
    style transfer; combining images ("put the character from image 2 into the scene of image 1"). Refer to the
    inputs as "image 1" (main), "image 2", "image 3" in the prompt. Output size follows the main image (~1 MP).

    Args:
        prompt: the edit instruction.
        filename, subfolder, source: the main image already on the server (e.g. a generate_image result), or
        image_base64: upload a local image instead (PNG/JPEG bytes, base64).
        reference_images: optional, max 2, each {"filename", "subfolder", "source"} or {"image_base64"} -> image 2, 3.
        fast: true = 4-step Lightning (a few seconds); false = 40 steps, cfg 4 (best quality, slower).
        seed: -1 for random.
        negative_prompt: things to avoid (only used when fast=false).
        transparent: cut the result out with BiRefNet and save an RGBA PNG (sprites, icons).
        filename_prefix: name stem for saved files (ComfyUI/output/mcp/).
        project: the game this is for (default: the active project); the result is logged under it.
        match_style: use the project's style lock. With a main image, the style anchor is added as the last
            reference (so at most 1 reference_images) and the prompt asks to keep its art style. With NO main image,
            the anchor becomes image 1 and a new subject (your prompt) is drawn in its style. The project's
            style_prompt is added either way.
    """
    _, ctx = use_project(project)
    style = (ctx or {}).get("style") or {}
    refs = list(reference_images or [])
    if len(refs) > 2:
        raise ToolError("At most 2 reference_images (image 2 and image 3).")
    has_main = bool(filename or image_base64)
    anchor_ref = None
    if match_style:
        if not style.get("anchor") and not style.get("prompt"):
            raise ToolError("match_style needs a project with a style_anchor or style_prompt (set_project_context).")
        if style.get("anchor"):
            anchor_ref = ensure_anchor_input(style["anchor"])
            if has_main and len(refs) > 1:
                raise ToolError("match_style uses one reference slot for the style anchor: pass at most 1 reference_images.")
        elif not has_main:
            raise ToolError("This project has no style_anchor, so edit_image needs a main image (filename or image_base64).")
    m = await edit_models()
    instruction = prompt
    if anchor_ref and not has_main:
        main = anchor_ref
        prompt = (f"Create a new image in exactly the art style of image 1 (same rendering, line work, shading and "
                  f"colour palette) but with a completely new subject: {prompt}. Do not copy the subject or "
                  f"composition of image 1.")
        if transparent:
            prompt += " Draw it on a plain solid white background."
    else:
        main = await input_image_ref(filename, subfolder, source, image_base64)
    ref_names = [await input_image_ref(r.get("filename", ""), r.get("subfolder", ""), r.get("source", "output"),
                                       r.get("image_base64", "")) for r in refs]
    if anchor_ref and has_main:
        ref_names.append(anchor_ref)
        prompt += (f" Keep the art style of image {len(ref_names) + 1} (rendering, line work, shading and colour "
                   f"palette).")
    if match_style and style.get("prompt"):
        prompt += f" Art style: {style['prompt']}."
    seed = random.randint(0, 2**50) if seed is None or seed < 0 else seed
    safe_prefix = "".join(c for c in filename_prefix if c.isalnum() or c in "-_") or "edit"
    g = build_edit_graph(m, main, ref_names, prompt, negative_prompt, seed, fast, transparent, f"mcp/{safe_prefix}")
    prompt_id, entry, elapsed = await queue_and_wait(g)
    images = await collect_images(entry, primary_node="save")
    summary = {"prompt_id": prompt_id, "prompt": instruction, "prompt_used": prompt, "seed": seed, "fast": fast,
               "inputs": [main, *ref_names], "match_style": match_style,
               "style_anchor": (style.get("anchor") or {}).get("file") if anchor_ref else None,
               "transparent": transparent, "seconds": round(elapsed, 1)}
    return await build_result(summary, images, preview)


REPAINT_MAX_FRAMES = 64


def _frame_size(data: bytes, what: str) -> tuple[int, int]:
    """Pixel size of a single image as ComfyUI's LoadImage will see it (turned upright)."""
    try:
        im = PILImage.open(io.BytesIO(data))
        several = getattr(im, "n_frames", 1) > 1
        size = ImageOps.exif_transpose(im).size
    except Exception:  # noqa: BLE001
        raise ToolError(f"{what} is not an image file that can be read.") from None
    if several:
        raise ToolError(f"{what} is an animated file with several frames: send single images.")
    return size


def _contact_strip(frames: list[bytes], columns: int = 8, cell: int = 192) -> PILImage.Image:
    """Every frame side by side on grey, in order, in rows of `columns`: one picture to judge an action by."""
    ims = []
    for data in frames:
        im = PILImage.open(io.BytesIO(data)).convert("RGBA")
        im.thumbnail((cell, cell))
        ims.append(im)
    rows = (len(ims) + columns - 1) // columns
    sheet = PILImage.new("RGBA", (min(len(ims), columns) * cell, rows * cell), (128, 128, 128, 255))
    for i, im in enumerate(ims):
        sheet.alpha_composite(im, ((i % columns) * cell + (cell - im.width) // 2, (i // columns) * cell + (cell - im.height) // 2))
    return sheet.convert("RGB")


@mcp.tool(structured_output=False)
async def repaint_frames(
    prompt: str,
    frames: list[dict[str, str]],
    reference_images: list[dict[str, str]] | None = None,
    fast: bool = True,
    seed: int = -1,
    negative_prompt: str = "",
    transparent: bool = True,
    filename_prefix: str = "frames",
    preview: bool = True,
    project: str = "",
) -> CallToolResult:
    """Repaint a whole action's pose-guide frames in one call: frames in, repainted frames out, in the same order
    (Qwen-Image-Edit-2511, the edit_image model, run once per frame with the same settings).

    For animation made from a rig: export the action as pose-guide frames (the character posed by the rig, stiff
    joints and all), send them with upload_link, and get back one finished frame per guide. Each guide is "image 1"
    of its own edit; the same instruction, references and seed are used for every frame.

    Args:
        prompt: the instruction for EVERY frame. It should say what image 1 is and what to keep, for example:
            "Image 1 is a rough paper cut-out figure made of stiff separate pieces. Repaint it as one clean finished
            hand-painted frame of the same character. Do not change the pose: the legs, knees and feet must stay
            exactly where they are in image 1. If the feet are together in image 1 they stay together; if they are
            apart they stay apart by the same distance. The arms stay where they are. Keep exactly the same size
            and position in the frame, the same face, hair, outfit and colours. Only fix the stiff joints, without
            moving them. Full body, plain solid white background, no shadow." Spell out what must not move and
            leave the action unnamed: a prompt that named it ("mid-walk") and only said "keep exactly the same
            pose" lost the feet-together frames of a walk cycle; this wording, on the same guides, kept them.
            {n} becomes the frame's number and {count} the number of frames ("frame {n} of {count}").
        frames: the guide frames in order, 1 to 64, each {"filename", "subfolder", "source"} (a file on the server:
            an upload_link file has source="input") or {"image_base64"}. All must be the same pixel size.
        reference_images: optional, max 2, used for every frame as image 2 and image 3 (the character's painting,
            a style reference): refer to them that way in the prompt.
        fast: true = 4-step Lightning (a few seconds a frame); false = 40 steps, cfg 4 (best quality, about 2
            minutes a frame).
        seed: one seed for every frame; -1 picks one at random. The result shows it, so a run can be repeated.
        negative_prompt: things to avoid (only used when fast=false).
        transparent: cut each frame out with BiRefNet and save an RGBA PNG (default true: sprites); the un-cut frame
            comes back as role "secondary".
        filename_prefix: name stem; frame 3 is saved as <prefix>_f03_....png (ComfyUI/output/mcp/).
        preview: return one contact strip of all the frames.
        project: the game this is for (default: the active project); every frame is logged under it.
    Result: "frames" lists every frame by number with its files; frame_size is the size of the repainted frames
    (the model works near 1 megapixel, so it can differ from guide_size; every frame has the same size, and
    pack_spritesheet's cell_size scales them). If a frame fails, the call stops there: "complete" is false,
    "stopped_at" names the frame and "error" says why; the frames before it are saved and recorded (a failure on
    the first frame is a plain error, since nothing was made). Each frame is
    recorded as it finishes, so a cancelled job keeps what it made. For fast=false or more than a few frames, run it
    through start_job.
    """
    use_project(project)
    refs = list(reference_images or [])
    if not frames:
        raise ToolError("Give at least one frame.")
    if len(frames) > REPAINT_MAX_FRAMES:
        raise ToolError(f"At most {REPAINT_MAX_FRAMES} frames in one call: split a longer action.")
    if len(refs) > 2:
        raise ToolError("At most 2 reference_images (image 2 and image 3).")
    frame_data = [(await read_ref(r))[0] for r in frames]
    sizes = [await asyncio.to_thread(_frame_size, d, f"Frame {i}") for i, d in enumerate(frame_data, start=1)]
    if len(set(sizes)) > 1:
        raise ToolError("The frames must all be the same pixel size, or the repainted frames will not line up. Sizes: "
                        + ", ".join(f"{i}: {w}x{h}" for i, (w, h) in enumerate(sizes, start=1)))
    ref_data = [(await read_ref(r))[0] for r in refs]
    for i, d in enumerate(ref_data, start=1):
        await asyncio.to_thread(_frame_size, d, f"Reference image {i}")
    m = await edit_models()

    async def name_of(r: dict[str, str]) -> str:
        return await input_image_ref(r.get("filename", ""), r.get("subfolder", ""), r.get("source", "output"),
                                     r.get("image_base64", ""))

    def hashed(name: str, data: bytes) -> dict[str, str]:
        return {"file": name, "sha256": hashlib.sha256(data).hexdigest()}

    frame_names = [await name_of(r) for r in frames]
    ref_log = [hashed(await name_of(r), d) for r, d in zip(refs, ref_data)]
    ref_names = [r["file"] for r in ref_log]
    seed = random.randint(0, 2**50) if seed is None or seed < 0 else seed
    prefix = _safe_name(filename_prefix, "frames")
    negative = "" if fast else negative_prompt
    done: list[dict[str, Any]] = []
    stopped: str | None = None
    t0 = time.time()
    for i, (name, data) in enumerate(zip(frame_names, frame_data), start=1):
        used = prompt.replace("{n}", str(i)).replace("{count}", str(len(frames)))
        g = build_edit_graph(m, name, ref_names, used, negative_prompt, seed, fast, transparent, f"mcp/{prefix}_f{i:02d}")
        try:
            prompt_id, entry, elapsed = await queue_and_wait(g)
            images = await collect_images(entry, primary_node="save")
            if not images:
                raise ToolError(f"ComfyUI finished prompt {prompt_id} without an image.")
        except Exception as e:  # noqa: BLE001  whatever went wrong, the frames already made must still be returned
            if i == 1:
                raise
            stopped = f"Frame {i}: {e}" if isinstance(e, ToolError) else f"Frame {i}: {type(e).__name__}: {e}"
            break
        # One record per frame, written as it lands: a cancelled job or a restart keeps what was made.
        log_asset("repaint_frames", {
            "action": prefix, "frame": i, "frames": len(frames), "prompt_id": prompt_id, "prompt": prompt,
            "prompt_used": used, "negative_used": negative or None, "seed": seed, "fast": fast, "transparent": transparent,
            "inputs": {"frame": hashed(name, data), "references": ref_log}, "seconds": round(elapsed, 1)}, images)
        done.append({"frame": i, "input": name, "seconds": round(elapsed, 1), "images": images})
    summary: dict[str, Any] = {"complete": stopped is None, "frames_done": len(done), "frames_asked": len(frames)}
    if stopped is not None:
        summary.update(stopped_at=len(done) + 1, error=stopped)
    first = [im for im in done[0]["images"] if im["role"] == "primary"][:1]
    out_size = None
    try:
        out_data, _ = await read_ref(filename=first[0]["filename"], subfolder=first[0]["subfolder"], source="output")
        out_size = list(await asyncio.to_thread(_frame_size, out_data, "The first repainted frame"))
    except Exception as e:  # noqa: BLE001  the size is a convenience; failing to read it must not lose the result
        log(f"repaint_frames could not read the size of its first frame: {e!r}")
    project_name, version = _log_project()
    summary.update({
        "prompt": prompt, "seed": seed, "fast": fast, "negative_used": negative or None, "transparent": transparent,
        "guide_size": list(sizes[0]), "frame_size": out_size, "references": ref_names,
        "seconds": round(time.time() - t0, 1), "project": project_name, "project_context_version": version,
        "frames": done,
        "download_hint": 'curl -o <file> "<url>" for each frame; pass the primary files to pack_spritesheet as frames.',
    })
    if negative_prompt and not negative:
        summary["negative_ignored"] = negative_prompt  # the 4-step model runs at CFG 1
    content: list[Any] = [TextContent(type="text", text=json.dumps(summary, indent=2, ensure_ascii=False))]
    if preview:
        try:
            shots = [(await read_ref(filename=im["filename"], subfolder=im["subfolder"], source="output"))[0]
                     for d in done for im in d["images"] if im["role"] == "primary"]
            strip = await asyncio.to_thread(_contact_strip, shots)
            content.append(png_preview(strip, max_side=1536))
        except Exception as e:  # noqa: BLE001  the frames are saved and recorded; a preview must not lose them
            log(f"repaint_frames preview failed: {e!r}")
    return CallToolResult(content=content)


# ---------------------------------------------------------------------------
# Animation: a character repainted along a clip (Wan-Animate-2)
# ---------------------------------------------------------------------------
# File names as in ComfyUI's video_wan_animate2 templates (Comfy-Org/Wan-Animate-2, Apache-2.0).
ANIMATE_MODELS: dict[bool, dict[str, Any]] = {
    False: {"unet": "wan_animate_2_distill_int8_convrot.safetensors", "lora": None, "steps": 10},
    True: {"unet": "wan_animate_2_int8_convrot.safetensors", "steps": 6,
           "lora": "lightx2v_I2V_14B_480p_cfg_step_distill_rank64_bf16.safetensors"},
}
ANIMATE_SHARED = {"clip": "umt5_xxl_fp8_e4m3fn_scaled.safetensors", "vae": "Wan2_1_VAE_bf16.safetensors",
                  "clip_vision": "clip_vision_h.safetensors"}
# The templates' negative prompt (both modes sample at CFG 1, where it has no effect; kept so the graph matches them).
WAN_NEGATIVE = "色调艳丽，过曝，静态，细节模糊不清，字幕，风格，作品，画作，画面，静止，整体发灰，最差质量，低质量，JPEG压缩残留，丑陋的，残缺的，多余的手指，画得不好的手部，画得不好的脸部，畸形的，毁容的，形态畸形的肢体，手指融合，静止不动的画面，杂乱的背景，三条腿，背景人很多，倒着走"
ANIMATE_MAX_FRAMES = 49
ANIMATE_MAX_FRAME_PIXELS = 49 * 640 * 640  # frames x width x height
ANIMATE_MAX_CLIP_BYTES = 64 * 1024 * 1024
ANIMATE_MAX_SOURCE_SIDE = 4096   # px, of a driving frame or the character image as sent
ANIMATE_SIZES = (320, 832)       # the longer side of a frame, in px
ANIMATE_CLIP_TYPES = (".mp4", ".mov")  # ComfyUI reads the frame count of these from the header; of webm and mkv it does not
_last_prompt: contextvars.ContextVar[str] = contextvars.ContextVar("last_prompt", default="")


async def _resolve_files(names: dict[str, str | None], what: str) -> dict[str, str | None]:
    """Match model file names against what ComfyUI can load; a missing one is an error that names it."""
    kinds = {"unet": "diffusion_models", "high": "diffusion_models", "low": "diffusion_models", "clip": "text_encoders",
             "vae": "vae", "lora": "loras", "lora_high": "loras", "lora_low": "loras", "clip_vision": "clip_vision"}
    lists = await model_lists()
    failed = [e for k in names for e in lists[kinds[k]] if e.startswith("<error")]
    if failed:
        raise ToolError(f"ComfyUI did not list its model files ({failed[0][:200]}). It may be busy: try again shortly.")
    out = {k: _resolve(v, lists[kinds[k]]) if v else None for k, v in names.items()}
    missing = [names[k] for k, v in out.items() if names[k] and v is None]
    if missing:
        raise ToolError(f"{what} is missing model files on the server: {missing}")
    return out


async def video_info(annotated: str) -> dict[str, Any]:
    """Width, height, frame count and frame rate of a video file ComfyUI can load, without decoding its frames."""
    g = {"clip": {"class_type": "LoadVideo", "inputs": {"file": annotated}},
         "show": {"class_type": "PreviewAny", "inputs": {"source": ["clip", 0]}}}
    _, entry, _ = await queue_and_wait(g)
    for item in entry.get("outputs", {}).get("clip", {}).get("images", []):
        meta = item.get("metadata") or {}
        if meta.get("frame_count") and meta.get("width") and meta.get("height"):
            return meta
    raise ToolError("ComfyUI could not read that file as a video.")


async def run_animation(tool: str, g: dict[str, Any], summary: dict[str, Any]) -> tuple[str, dict[str, Any], float]:
    """Queue an animation graph and wait for it. If it fails or is cancelled after ComfyUI has saved some of its
    files, record those (marked incomplete): a saved file belongs in the authorship record whatever happened next."""
    _last_prompt.set("")
    try:
        return await queue_and_wait(g, timeout_s=max(DEFAULT_TIMEOUT_S, 1800))
    except BaseException as failure:
        prompt_id = _last_prompt.get()
        try:
            if prompt_id:
                # A background cancellation owns upstream requests; a direct tool call may
                # still request a targeted stop without using ComfyUI's global interrupt.
                job = _current_job.get()
                if job is None or job["status"] != "cancel_requested":
                    if await _request_prompt_cancel(prompt_id) == "unsupported":
                        await _dequeue(prompt_id)
            entry = None
            for _ in range(8 if prompt_id else 0):
                entry = (await _http.get(f"/history/{prompt_id}", timeout=5)).json().get(prompt_id)
                if entry:
                    break
                await asyncio.sleep(1)
            saved = [f for f in await collect_images(entry or {}) if f["type"] == "output"]
            if saved:
                log_asset(tool, {**summary, "prompt_id": prompt_id, "incomplete": True,
                                 "error": f"{type(failure).__name__}: {failure}"[:500]}, saved)
        except Exception as e:  # noqa: BLE001  the first failure is the one to report
            log(f"{tool}: could not record what a failed run saved: {e!r}")
        raise


def _add_frame_outputs(g: dict[str, Any], images: list[Any], indices: list[int], prefix: str, transparent: bool,
                       fps: float) -> list[str]:
    """Save chosen frames of an IMAGE batch, one file each (<prefix>_f01, _f02 ...), and the whole batch as an mp4.

    Frames are cut out one at a time when transparent: a cut-out of a whole clip at once is what filled the RAM.
    Returns the ids of the frame save nodes, in order; the video's save node is "save_video"."""
    g["video"] = {"class_type": "CreateVideo", "inputs": {"images": images, "fps": fps}}
    g["save_video"] = {"class_type": "SaveVideo", "inputs": {"video": ["video", 0], "filename_prefix": prefix,
                                                             "format": "mp4", "codec": "h264"}}
    if transparent:
        g["bg_model"] = {"class_type": "LoadBackgroundRemovalModel", "inputs": {"bg_removal_name": BG_REMOVAL_MODEL}}
    saves = []
    for n, index in enumerate(indices, start=1):
        g[f"pick{n}"] = {"class_type": "ImageFromBatch", "inputs": {"image": images, "batch_index": index, "length": 1}}
        frame: list[Any] = [f"pick{n}", 0]
        if transparent:
            g[f"mask{n}"] = {"class_type": "RemoveBackground", "inputs": {"bg_removal_model": ["bg_model", 0], "image": frame}}
            g[f"inv{n}"] = {"class_type": "InvertMask", "inputs": {"mask": [f"mask{n}", 0]}}
            g[f"rgba{n}"] = {"class_type": "JoinImageWithAlpha", "inputs": {"image": frame, "alpha": [f"inv{n}", 0]}}
            frame = [f"rgba{n}", 0]
        g[f"save{n}"] = {"class_type": "SaveImage", "inputs": {"images": frame, "filename_prefix": f"{prefix}_f{n:02d}"}}
        saves.append(f"save{n}")
    return saves


async def _frames_result(tool: str, summary: dict[str, Any], entry: dict[str, Any], saves: list[str],
                         preview: bool) -> CallToolResult:
    """Record one animation call and build its result: the frames by number, the video, one contact strip."""
    by_node: dict[str, dict[str, Any]] = {}
    for im in await collect_images(entry):
        by_node.setdefault(im["node"], im)
    frames = [by_node[s] for s in saves if s in by_node]
    video = by_node.get("save_video")
    if len(frames) != len(saves):
        if frames or video:
            log_asset(tool, {**summary, "incomplete": True}, frames + ([video] if video else []))
        raise ToolError(f"ComfyUI saved {len(frames)} of {len(saves)} frames; they are recorded as an incomplete run.")
    log_asset(tool, summary, frames + ([video] if video else []))  # stamps the project into summary
    # One address per frame keeps a long action's result short; "bases" lists the others.
    summary["frames"] = [{"frame": n, "filename": f["filename"], "subfolder": f["subfolder"], "type": f["type"], "url": f["url"]}
                         for n, f in enumerate(frames, start=1)]
    summary["video"] = {k: video[k] for k in ("filename", "subfolder", "type", "url", "urls")} if video else None
    summary["bases"] = PUBLIC_URLS
    summary["download_hint"] = ('curl -o <file> "<url>" for each frame and the video ("bases" lists every address the server '
                                "is configured to publish). Pass the frames to pack_spritesheet.")
    content: list[Any] = [TextContent(type="text", text=json.dumps(summary, indent=2, ensure_ascii=False))]
    if preview:
        try:
            shots = [(await read_ref(filename=f["filename"], subfolder=f["subfolder"], source="output"))[0] for f in frames]
            content.append(png_preview(await asyncio.to_thread(_contact_strip, shots), max_side=1536))
        except Exception as e:  # noqa: BLE001  the frames are saved and recorded; a preview must not lose them
            log(f"{tool} preview failed: {e!r}")
    return CallToolResult(content=content)


def _fit_size(width: int, height: int, longer: int) -> tuple[int, int]:
    """Scale a size so its longer side is about `longer`, both sides multiples of 16."""
    k = longer / max(width, height)
    return max(16, round(width * k / 16) * 16), max(16, round(height * k / 16) * 16)


@mcp.tool(structured_output=False)
async def animate_from_clip(
    prompt: str,
    filename: str = "",
    subfolder: str = "",
    source: str = "output",
    image_base64: str = "",
    clip: dict[str, str] | None = None,
    frames: list[dict[str, str]] | None = None,
    motion_prompt: str = "",
    background: str = "a plain solid white background, nothing else",
    size: int = 640,
    quality: bool = False,
    seed: int = -1,
    transparent: bool = True,
    fps: float = 16.0,
    filename_prefix: str = "anim",
    preview: bool = True,
    project: str = "",
) -> CallToolResult:
    """Repaint a character along a clip: one image of the character plus a short driving clip in, one frame per
    clip frame out (Wan-Animate-2, Apache-2.0; no pose extraction, the clip's own frames drive the motion).

    For animation made from a rig: export the action as a short clip (or as its frames), give the character's
    painting as the image, and get the character performing the clip's motion. The clip can show the rig's stiff
    puppet; the output takes its motion, not its look.

    Args:
        prompt: what the character looks like, in words ("a young cultivator in a knee-length pale robe with teal
            trim, hand-painted 2D game art"). The model needs this as well as the image.
        filename, subfolder, source: the character's image on the server (a result, or an upload_link file with
            source="input"), or
        image_base64: a small local image instead. Frame it like the clip (full body in both).
        clip: the driving video, {"filename", "subfolder", "source"} of an mp4 or mov file sent with upload_link
            (other formats: send the frames), or
        frames: the driving clip as images in order, each {"filename", "subfolder", "source"} or {"image_base64"},
            all the same pixel size. Give clip or frames, not both. At most 49 frames either way (3 seconds at
            16 fps), fewer at a size above 640: the result has one frame per driving frame. A few key poses work, but the model treats them
            as a video and moves smoothly between them, so a dense clip follows a rig more closely.
        motion_prompt: optional, what the clip shows ("a figure walking in place, arms swinging").
        background: what is behind the character; keep the plain white default for cut-outs.
        size: longer side of the frames in px, 320 to 832 (default 640); the shape follows the clip's.
        quality: false = the distilled model, 10 steps (35 to 50 s for 33 frames once loaded); true = the full
            model with its 6-step LoRA.
        seed: -1 for random.
        transparent: cut every frame out with BiRefNet and save RGBA PNGs (default true: sprites).
        fps: frame rate of the saved video when the clip is given as frames (a video clip keeps its own).
        filename_prefix: name stem; frame 3 is saved as <prefix>_f03_....png, the video as <prefix>_....mp4.
        preview: return one contact strip of the frames.
        project: the game this is for (default: the active project); the result is logged under it.
    Result: "frames" lists every frame by number (file and one address), "video" is the un-cut result as an mp4 to
    watch. It promises frames and a video, not that the character or the motion came out right: look at the strip
    and the video. Run it through start_job: a first call loads about 25 GB of models.
    """
    use_project(project)
    if (clip is None) == (not frames):
        raise ToolError("Give the driving clip either as clip (a video file) or as frames (images in order), not both.")
    if not ANIMATE_SIZES[0] <= size <= ANIMATE_SIZES[1]:
        raise ToolError(f"size is the longer side of a frame in px, {ANIMATE_SIZES[0]} to {ANIMATE_SIZES[1]}.")
    if not 1.0 <= fps <= 120.0:
        raise ToolError("fps must be between 1 and 120.")
    too_long = f"The clip has {{n}} frames. At most {ANIMATE_MAX_FRAMES} are supported per call. Split the clip."
    ref_data, _ = await read_ref(filename=filename, subfolder=subfolder, source=source, data_base64=image_base64)
    too_big = f"{{what}} is {{w}}x{{h}} px; at most {ANIMATE_MAX_SOURCE_SIDE} px a side."
    ref_w, ref_h = await asyncio.to_thread(_frame_size, ref_data, "The character image")
    if max(ref_w, ref_h) > ANIMATE_MAX_SOURCE_SIDE:
        raise ToolError(too_big.format(what="The character image", w=ref_w, h=ref_h))
    files = await _resolve_files({**ANIMATE_SHARED, "unet": ANIMATE_MODELS[bool(quality)]["unet"],
                                  "lora": ANIMATE_MODELS[bool(quality)]["lora"]}, "animate_from_clip")

    def hashed(name: str, data: bytes) -> dict[str, str]:
        return {"file": name, "sha256": hashlib.sha256(data).hexdigest()}

    g: dict[str, Any] = {}
    if clip is not None:
        name = clip.get("filename", "")
        if not name or clip.get("image_base64") or clip.get("data_base64"):
            raise ToolError("Send the clip with upload_link and pass its filename, subfolder and source.")
        if not name.lower().endswith(ANIMATE_CLIP_TYPES):
            raise ToolError("The clip must be an mp4 or mov file (ComfyUI cannot read the length of other formats up front). "
                            "Export it as mp4, or pass the action as frames.")
        clip_data, _ = await read_ref(filename=name, subfolder=clip.get("subfolder", ""), source=clip.get("source", "output"),
                                      max_bytes=ANIMATE_MAX_CLIP_BYTES)
        clip_name = await input_image_ref(name, clip.get("subfolder", ""), clip.get("source", "output"), "")
        info = await video_info(clip_name)
        count, src_w, src_h = int(info["frame_count"]), int(info["width"]), int(info["height"])
        fps = min(120.0, max(1.0, float(info.get("fps") or fps)))  # the saved video keeps the clip's frame rate
        if count > ANIMATE_MAX_FRAMES:
            raise ToolError(too_long.format(n=count))
        g["clip_file"] = {"class_type": "LoadVideo", "inputs": {"file": clip_name}}
        g["clip_frames"] = {"class_type": "GetVideoComponents", "inputs": {"video": ["clip_file", 0]}}
        driving: list[Any] = ["clip_frames", 0]
        driving_log: Any = hashed(clip_name, clip_data)
    else:
        assert frames is not None
        if len(frames) > ANIMATE_MAX_FRAMES:
            raise ToolError(too_long.format(n=len(frames)))
        frame_data = [(await read_ref(r))[0] for r in frames]
        sizes = [await asyncio.to_thread(_frame_size, d, f"Frame {i}") for i, d in enumerate(frame_data, start=1)]
        if len(set(sizes)) > 1:
            raise ToolError("The frames must all be the same pixel size. Sizes: "
                            + ", ".join(f"{i}: {w}x{h}" for i, (w, h) in enumerate(sizes, start=1)))
        count, (src_w, src_h) = len(frames), sizes[0]
    if max(src_w, src_h) > ANIMATE_MAX_SOURCE_SIDE:
        raise ToolError(too_big.format(what="The clip", w=src_w, h=src_h))
    width, height = _fit_size(src_w, src_h, size)
    length = 4 * math.ceil((count - 1) / 4) + 1  # the model makes 4n+1 frames; the extra ones are not saved
    if length * width * height > ANIMATE_MAX_FRAME_PIXELS:
        most = (ANIMATE_MAX_FRAME_PIXELS // (width * height) - 1) // 4 * 4 + 1
        raise ToolError(f"{count} frames at {width}x{height} px exceed the per-call limit of {most} frames at that size. "
                        "Use a smaller size or a shorter clip.")
    if frames:  # everything is checked: only now is anything uploaded
        names = [await input_image_ref(r.get("filename", ""), r.get("subfolder", ""), r.get("source", "output"),
                                       r.get("image_base64", "")) for r in frames]
        joined: dict[str, Any] = {}
        for i, name in enumerate(names):
            g[f"in{i + 1}"] = {"class_type": "LoadImage", "inputs": {"image": name}}
            joined[f"images.image{i}"] = [f"in{i + 1}", 0]
        g["frames_in"] = {"class_type": "BatchImagesNode", "inputs": joined}  # one batch, as the templates build it
        driving = ["frames_in", 0]
        driving_log = [hashed(n, d) for n, d in zip(names, frame_data)]
    reference = await input_image_ref(filename, subfolder, source, image_base64)
    seed = random.randint(0, 2**50) if seed is None or seed < 0 else seed
    prefix = _safe_name(filename_prefix, "anim")
    character = f"Character appearance description: {prompt.strip()}\nBackground description: {background.strip()}"
    mode = ANIMATE_MODELS[bool(quality)]

    def scaled(image: list[Any]) -> dict[str, Any]:
        return {"class_type": "ImageScale", "inputs": {"image": image, "upscale_method": "area", "width": width,
                                                        "height": height, "crop": "center"}}

    g.update({
        "unet": {"class_type": "UNETLoader", "inputs": {"unet_name": files["unet"], "weight_dtype": "default"}},
        "clip": {"class_type": "CLIPLoader", "inputs": {"clip_name": files["clip"], "type": "wan", "device": "default"}},
        "vae": {"class_type": "VAELoader", "inputs": {"vae_name": files["vae"]}},
        "vision": {"class_type": "CLIPVisionLoader", "inputs": {"clip_name": files["clip_vision"]}},
        "character": {"class_type": "LoadImage", "inputs": {"image": reference}},
        "character_fit": scaled(["character", 0]),
        "driving_fit": scaled(driving),
        "driving_first": {"class_type": "ImageFromBatch", "inputs": {"image": ["driving_fit", 0], "batch_index": 0, "length": 1}},
        "see_character": {"class_type": "CLIPVisionEncode", "inputs": {"clip_vision": ["vision", 0], "image": ["character_fit", 0],
                                                                       "crop": "none"}},
        "see_driving": {"class_type": "CLIPVisionEncode", "inputs": {"clip_vision": ["vision", 0], "image": ["driving_first", 0],
                                                                     "crop": "none"}},
        "pos": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["clip", 0], "text": character}},
        "neg": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["clip", 0], "text": WAN_NEGATIVE}},
        "motion": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["clip", 0], "text": motion_prompt.strip()}},
    })
    model: list[Any] = ["unet", 0]
    if files["lora"]:
        g["lora"] = {"class_type": "LoraLoaderModelOnly", "inputs": {"model": model, "lora_name": files["lora"],
                                                                       "strength_model": 1.0}}
        model = ["lora", 0]
    g.update({
        "cache": {"class_type": "WanAnimate2Cache", "inputs": {"model": model, "device": "gpu", "dtype": "int8"}},
        "shift": {"class_type": "ModelSamplingSD3", "inputs": {"model": ["cache", 0], "shift": 5.0}},
        "animate": {"class_type": "WanAnimate2ToVideo", "inputs": {
            "positive": ["pos", 0], "negative": ["neg", 0], "vae": ["vae", 0], "width": width, "height": height,
            "length": length, "batch_size": 1, "video_frame_offset": 0, "pose_strength": 1.0, "pose_start_percent": 0.0,
            "pose_end_percent": 1.0, "reference_image_strength": 1.0, "reference_image": ["character_fit", 0],
            "pose_video": ["driving_fit", 0], "clip_vision_output": ["see_character", 0], "positive_pose": ["motion", 0],
            "clip_vision_output_pose": ["see_driving", 0]}},
        "sigmas": {"class_type": "BasicScheduler", "inputs": {"model": ["cache", 0], "scheduler": "simple",
                                                              "steps": mode["steps"], "denoise": 1.0}},
        "sampler_kind": {"class_type": "KSamplerSelect", "inputs": {"sampler_name": "lcm"}},
        "sampler": {"class_type": "SamplerCustom", "inputs": {
            "model": ["shift", 0], "add_noise": True, "noise_seed": seed, "cfg": 1.0, "positive": ["animate", 0],
            "negative": ["animate", 1], "sampler": ["sampler_kind", 0], "sigmas": ["sigmas", 0], "latent_image": ["animate", 2]}},
        "trim": {"class_type": "TrimVideoLatent", "inputs": {"samples": ["sampler", 0], "trim_amount": ["animate", 3]}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["trim", 0], "vae": ["vae", 0]}},
        "kept": {"class_type": "ImageFromBatch", "inputs": {"image": ["decode", 0], "batch_index": 0, "length": count}},
    })
    saves = _add_frame_outputs(g, ["kept", 0], list(range(count)), f"mcp/{prefix}", transparent, fps)
    summary = {
        "prompt": prompt, "prompt_used": character, "motion_prompt": motion_prompt.strip() or None,
        "negative_used": WAN_NEGATIVE, "seed": seed, "preset": "wan_animate_2" if quality else "wan_animate_2_distilled",
        "model": files["unet"], "lora": files["lora"], "models": {k: v for k, v in files.items() if v},
        "steps": mode["steps"], "cfg": 1.0, "frame_count": count, "frame_size": [width, height],
        "driving_size": [src_w, src_h], "fps": fps, "transparent": transparent,
        "inputs": {"character": hashed(reference, ref_data), "clip" if clip is not None else "frames": driving_log},
    }
    prompt_id, entry, elapsed = await run_animation("animate_from_clip", g, summary)
    summary = {"prompt_id": prompt_id, **summary, "seconds": round(elapsed, 1)}
    return await _frames_result("animate_from_clip", summary, entry, saves, preview)


# ---------------------------------------------------------------------------
# Animation: a loop from one image (Wan 2.2 image-to-video, first and last frame pinned)
# ---------------------------------------------------------------------------
# File names as in ComfyUI's video_wan2_2_14B_flf2v template (Comfy-Org/Wan_2.2_ComfyUI_Repackaged, Apache-2.0).
LOOP_MODELS = {"high": "wan2.2_i2v_high_noise_14B_fp8_scaled.safetensors", "low": "wan2.2_i2v_low_noise_14B_fp8_scaled.safetensors",
               "clip": "umt5_xxl_fp8_e4m3fn_scaled.safetensors", "vae": "wan_2.1_vae.safetensors"}
LOOP_LORAS = {"lora_high": "wan2.2_i2v_lightx2v_4steps_lora_v1_high_noise.safetensors",
              "lora_low": "wan2.2_i2v_lightx2v_4steps_lora_v1_low_noise.safetensors"}
LOOP_FPS = 16.0              # the model's frame rate


def _has_transparency(data: bytes) -> bool:
    """Whether an image has any pixel that is not fully opaque."""
    im = PILImage.open(io.BytesIO(data))
    if im.mode not in ("RGBA", "LA", "PA") and "transparency" not in im.info:
        return False
    return im.convert("RGBA").getchannel("A").getextrema()[0] < 255
LOOP_MAX_VIDEO_FRAMES = 48  # frames × spacing


@mcp.tool(structured_output=False)
async def animate_loop(
    prompt: str,
    filename: str = "",
    subfolder: str = "",
    source: str = "output",
    image_base64: str = "",
    frames: int = 8,
    spacing: int = 4,
    size: int = 640,
    quality: bool = False,
    seed: int = -1,
    negative_prompt: str = "",
    transparent: bool = True,
    filename_prefix: str = "loop",
    preview: bool = True,
    project: str = "",
) -> CallToolResult:
    """Make a looping animation from ONE image: a short video that starts and ends on that image, cut into
    evenly spaced frames (Wan 2.2 image-to-video 14B, Apache-2.0, with the image pinned as first and last frame).

    For an idle, a walk in place, a flag, a torch: anything that should come back to where it started. The video
    model invents the motion from the prompt; nothing guides the poses. To follow a rig's poses use
    animate_from_clip or repaint_frames.

    Args:
        prompt: the motion, and what must not move: "The knight walks in place on the spot, legs stepping, arms
            swinging. He stays in the centre, facing the viewer. Plain solid white background, fixed camera, no
            camera movement."
        filename, subfolder, source: the image on the server (a result, or an upload_link file with
            source="input"), or
        image_base64: a small local image instead. Use the un-cut image on a plain background (a generate_image
            result's secondary file), not a transparent cut-out.
        frames: how many frames to keep (default 8).
        spacing: video frames between kept frames, 1 to 8 (default 4). The video is frames x spacing frames long
            at 16 fps, so 8 x 4 is a 2 second loop played at 4 kept frames a second; frames x spacing must be a
            multiple of 4 and at most 48 (fewer at a size above 640).
        size: longer side of the frames in px, 320 to 832 (default 640); the shape follows the image's.
        quality: false = 4 steps with the Lightning LoRAs (20 to 30 s once loaded); true = 20 steps, cfg 4
            (about 90 s, livelier motion, and takes a negative prompt).
        seed: -1 for random.
        negative_prompt: things to avoid (only used when quality=true; empty = the model's standard one).
        transparent: cut every kept frame out with BiRefNet and save RGBA PNGs (default true: sprites).
        filename_prefix: name stem; frame 3 is saved as <prefix>_f03_....png, the video as <prefix>_....mp4.
        preview: return one contact strip of the kept frames.
        project: the game this is for (default: the active project); the result is logged under it.
    Result: "frames" lists the kept frames by number (file and one address), "video" is the whole loop as an mp4
    to watch (the repeated last frame is left out of both, so they loop). It promises frames and a video, not
    that the loop is smooth or the character unchanged in the middle: look at the strip and the video. To pack
    the frames into a sprite sheet, pass them to pack_spritesheet as one animation with fps = the result's
    frames_fps and loop = true. Run it through start_job: a first call loads about 38 GB of models.
    """
    use_project(project)
    if not (1 <= frames <= LOOP_MAX_VIDEO_FRAMES and 1 <= spacing <= 8):
        raise ToolError(f"frames is 1 to {LOOP_MAX_VIDEO_FRAMES} and spacing 1 to 8.")
    video_frames = frames * spacing
    if video_frames % 4 or video_frames > LOOP_MAX_VIDEO_FRAMES:
        raise ToolError(f"frames x spacing is {video_frames}: it must be a multiple of 4 (the model makes 4n+1 frames) and at "
                        f"most {LOOP_MAX_VIDEO_FRAMES}. For example 8 x 4, 8 x 2, 12 x 4 or 6 x 2.")
    if not ANIMATE_SIZES[0] <= size <= ANIMATE_SIZES[1]:
        raise ToolError(f"size is the longer side of a frame in px, {ANIMATE_SIZES[0]} to {ANIMATE_SIZES[1]}.")
    image_data, _ = await read_ref(filename=filename, subfolder=subfolder, source=source, data_base64=image_base64)
    src_w, src_h = await asyncio.to_thread(_frame_size, image_data, "The image")
    if max(src_w, src_h) > ANIMATE_MAX_SOURCE_SIDE:
        raise ToolError(f"The image is {src_w}x{src_h} px; at most {ANIMATE_MAX_SOURCE_SIDE} px a side.")
    see_through = await asyncio.to_thread(_has_transparency, image_data)
    width, height = _fit_size(src_w, src_h, size)
    if (video_frames + 1) * width * height > ANIMATE_MAX_FRAME_PIXELS:
        raise ToolError(f"{video_frames} video frames at {width}x{height} px exceed the per-call limit. "
                        f"At this size frames x spacing can be at most "
                        f"{(ANIMATE_MAX_FRAME_PIXELS // (width * height) - 1) // 4 * 4}. Use a smaller size or a shorter loop.")
    files = await _resolve_files({**LOOP_MODELS, **(LOOP_LORAS if not quality else {})}, "animate_loop")
    image = await input_image_ref(filename, subfolder, source, image_base64)  # everything is checked: only now is it uploaded
    seed = random.randint(0, 2**50) if seed is None or seed < 0 else seed
    prefix = _safe_name(filename_prefix, "loop")
    # The template's two settings: 4 steps split at 2 with the Lightning LoRAs, or 20 steps split at 10 at cfg 4.
    steps, cfg, split, shift = (20, 4.0, 10, 8.0) if quality else (4, 1.0, 2, 5.0)
    negative = (negative_prompt.strip() or WAN_NEGATIVE) if quality else WAN_NEGATIVE
    g: dict[str, Any] = {
        "clip": {"class_type": "CLIPLoader", "inputs": {"clip_name": files["clip"], "type": "wan", "device": "default"}},
        "vae": {"class_type": "VAELoader", "inputs": {"vae_name": files["vae"]}},
        "high": {"class_type": "UNETLoader", "inputs": {"unet_name": files["high"], "weight_dtype": "default"}},
        "low": {"class_type": "UNETLoader", "inputs": {"unet_name": files["low"], "weight_dtype": "default"}},
        "image": {"class_type": "LoadImage", "inputs": {"image": image}},
        # The pin node's own resize is plain bilinear, which drops fine lines when a large image is shrunk: scale first.
        "image_fit": {"class_type": "ImageScale", "inputs": {"image": ["image", 0], "upscale_method": "area", "width": width,
                                                              "height": height, "crop": "center"}},
        "pos": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["clip", 0], "text": prompt}},
        "neg": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["clip", 0], "text": negative}},
        "pinned": {"class_type": "WanFirstLastFrameToVideo", "inputs": {
            "positive": ["pos", 0], "negative": ["neg", 0], "vae": ["vae", 0], "width": width, "height": height,
            "length": video_frames + 1, "batch_size": 1, "start_image": ["image_fit", 0], "end_image": ["image_fit", 0]}},
    }
    high, low = ["high", 0], ["low", 0]
    if not quality:
        g["high_lora"] = {"class_type": "LoraLoaderModelOnly", "inputs": {"model": high, "lora_name": files["lora_high"],
                                                                            "strength_model": 1.0}}
        g["low_lora"] = {"class_type": "LoraLoaderModelOnly", "inputs": {"model": low, "lora_name": files["lora_low"],
                                                                           "strength_model": 1.0}}
        high, low = ["high_lora", 0], ["low_lora", 0]
    g.update({
        "high_shift": {"class_type": "ModelSamplingSD3", "inputs": {"model": high, "shift": shift}},
        "low_shift": {"class_type": "ModelSamplingSD3", "inputs": {"model": low, "shift": shift}},
        "first_half": {"class_type": "KSamplerAdvanced", "inputs": {
            "model": ["high_shift", 0], "add_noise": "enable", "noise_seed": seed, "steps": steps, "cfg": cfg,
            "sampler_name": "euler", "scheduler": "simple", "positive": ["pinned", 0], "negative": ["pinned", 1],
            "latent_image": ["pinned", 2], "start_at_step": 0, "end_at_step": split, "return_with_leftover_noise": "enable"}},
        "second_half": {"class_type": "KSamplerAdvanced", "inputs": {
            "model": ["low_shift", 0], "add_noise": "disable", "noise_seed": 0, "steps": steps, "cfg": cfg,
            "sampler_name": "euler", "scheduler": "simple", "positive": ["pinned", 0], "negative": ["pinned", 1],
            "latent_image": ["first_half", 0], "start_at_step": split, "end_at_step": 10000,
            "return_with_leftover_noise": "disable"}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["second_half", 0], "vae": ["vae", 0]}},
        # the last frame repeats the first: leave it out, so the frames and the video loop
        "kept": {"class_type": "ImageFromBatch", "inputs": {"image": ["decode", 0], "batch_index": 0, "length": video_frames}},
    })
    saves = _add_frame_outputs(g, ["kept", 0], [i * spacing for i in range(frames)], f"mcp/{prefix}", transparent, LOOP_FPS)
    summary = {
        "prompt": prompt, "negative_used": negative if quality else None,
        "seed": seed, "preset": "wan2.2_i2v_loop" if quality else "wan2.2_i2v_loop_fast",
        "models": {k: v for k, v in files.items() if v}, "steps": steps, "cfg": cfg, "shift": shift,
        "frames_kept": frames, "spacing": spacing, "video_frames": video_frames,
        "video_fps": LOOP_FPS, "frames_fps": LOOP_FPS / spacing, "frame_size": [width, height], "image_size": [src_w, src_h],
        "transparent": transparent,
        "inputs": {"image": {"file": image, "sha256": hashlib.sha256(image_data).hexdigest()}},
    }
    if negative_prompt.strip() and not quality:
        summary["negative_ignored"] = negative_prompt.strip()  # the 4-step mode runs at CFG 1
    if see_through:
        summary["note"] = ("The image has transparent pixels: the model saw whatever colours lie under them. Use the un-cut "
                           "image on a plain background.")
    prompt_id, entry, elapsed = await run_animation("animate_loop", g, summary)
    summary = {"prompt_id": prompt_id, **summary, "seconds": round(elapsed, 1)}
    return await _frames_result("animate_loop", summary, entry, saves, preview)


# ---------------------------------------------------------------------------
# Region edit: change one area of an image and keep every other pixel
# ---------------------------------------------------------------------------
REGION_WORK_PIXELS = 1024 * 1024  # a small context crop is scaled up to about this many pixels for the model
REGION_MAX_PIXELS = 2048 * 1536   # a large one is scaled down to this many
REGION_MAX_UPSCALE = 4.0
REGION_MAX_ASPECT = 3.0           # a longer context crop gets its short side widened: the model needs a sane shape
REGION_MAX_FEATHER = 256          # a wider soft edge is never needed, and this bounds the work a call can ask for
REGION_MIN_BOX = 64               # and a smaller one is widened to this, so 4x reaches the model's smallest size
REGION_MAX_IMAGE_PIXELS = 8192 * 8192  # the whole image is held in memory, a few copies of it
REGION_PRESETS = {False: "flux3_klein", True: "flux3_klein_base"}


def _load_rgb(data: bytes, what: str) -> PILImage.Image:
    """Open an 8-bit image as RGB, or RGBA when it has transparency, turned the way ComfyUI's LoadImage turns it.

    An image that would lose information on the way to 8-bit RGB (16 bits a channel, float, CMYK) is refused: the
    promise that untouched pixels stay exactly as they were would be checked against a converted copy, not the file."""
    unreadable = ToolError(f"The {what} is not an image file that can be read.")
    try:
        im = PILImage.open(io.BytesIO(data))
        deep = any(";16" in str(t) for t in getattr(im, "tile", None) or [])  # a 16-bit RGB PNG opens as plain "RGB"
    except PILImage.DecompressionBombError:
        raise ToolError(f"The {what} is too large: edit_region takes images up to "
                        f"{REGION_MAX_IMAGE_PIXELS // 10**6} megapixels.") from None
    except Exception:  # noqa: BLE001
        raise unreadable from None
    if deep or im.mode not in ("1", "L", "LA", "P", "PA", "RGB", "RGBA"):
        raise ToolError(f"The {what} is not an 8-bit RGB, RGBA, grey or palette image (it is {im.mode}"
                        f"{', 16 bits a channel' if deep else ''}): convert it first.")
    if im.width * im.height > REGION_MAX_IMAGE_PIXELS:
        raise ToolError(f"The {what} is {im.width}x{im.height} px; edit_region takes images up to "
                        f"{REGION_MAX_IMAGE_PIXELS // 10**6} megapixels.")
    try:
        im = ImageOps.exif_transpose(im)
        has_alpha = im.mode in ("RGBA", "LA", "PA") or "transparency" in im.info
        return im.convert("RGBA" if has_alpha else "RGB")
    except Exception:  # noqa: BLE001
        raise unreadable from None


def _load_mask(data: bytes, size: tuple[int, int], what: str) -> PILImage.Image:
    """An 8-bit mask the size of the image: white = selected, black or transparent = not, grey = partly.

    Values under 8 count as black everywhere, so a mask saved as JPEG or with a not-quite-black background selects
    only what was painted."""
    im = _load_rgb(data, what)
    if im.size != size:
        raise ToolError(f"The {what} is {im.width}x{im.height} px but the image is {size[0]}x{size[1]} px: they must "
                        "be the same size.")
    m = im.convert("L")
    if im.mode == "RGBA":
        m = ImageChops.multiply(m, im.getchannel("A"))
    return m.point(lambda v: v if v >= 8 else 0)


def _trim(lo: int, hi: int, size: int, keep_lo: int, keep_hi: int) -> tuple[int, int]:
    """Shrink the span lo..hi to `size`, centred on the part keep_lo..keep_hi, which must fit in it."""
    if keep_hi - keep_lo > size:
        raise ToolError("The area is too long and thin for the model (more than 8 times longer than the image leaves "
                        "room to make it wide). Edit it in shorter pieces.")
    start = (keep_lo + keep_hi - size) // 2
    start = max(lo, keep_hi - size, min(start, keep_lo, hi - size))
    return start, start + size


def _widen(lo: int, hi: int, size: int, limit: int) -> tuple[int, int]:
    """Grow the span lo..hi to `size` around its centre, kept inside 0..limit."""
    if size >= limit:
        return 0, limit
    lo = max(0, min(limit - size, lo - (size - (hi - lo)) // 2))
    return lo, lo + size


def _pad_edges(im: PILImage.Image, left: int, top: int, right: int, bottom: int) -> PILImage.Image:
    """Pad an image by repeating its edge pixels."""
    if not (left or top or right or bottom):
        return im
    w, h = im.size
    out = PILImage.new(im.mode, (w + left + right, h + top + bottom))
    out.paste(im, (left, top))
    if left:
        out.paste(im.crop((0, 0, 1, h)).resize((left, h)), (0, top))
    if right:
        out.paste(im.crop((w - 1, 0, w, h)).resize((right, h)), (left + w, top))
    if top:
        out.paste(out.crop((0, top, out.width, top + 1)).resize((out.width, top)), (0, 0))
    if bottom:
        out.paste(out.crop((0, top + h - 1, out.width, top + h)).resize((out.width, bottom)), (0, top + h))
    return out


def _any_band(im: PILImage.Image) -> PILImage.Image:
    """One 8-bit band holding the largest value of all bands (getbbox on RGBA would look at alpha only)."""
    bands = im.split()
    out = bands[0]
    for b in bands[1:]:
        out = ImageChops.lighter(out, b)
    return out


def plan_region(src: PILImage.Image, region: dict[str, Any] | None, mask_data: bytes | None,
                protect_data: bytes | None, context: int, feather: int) -> dict[str, Any]:
    """Work out where a region edit happens: the context box the model sees, the size it works at, and the blend
    mask (the size of the context box; 0 = keep the original pixel exactly).

    A rectangle gets a feather that runs inward from its edge, so nothing outside it ever changes; at an image
    border the rectangle is treated as carrying on past it, so the edit does not fade out there. A mask is used as
    painted: the server cannot know how thick each part of a shape is, and a feather wide enough for a lake fades a
    river away, so the soft edge of a mask is the caller's to paint."""
    width, height = src.size
    mask = None
    if region is not None:
        try:
            x, y, w, h = (int(region[k]) for k in ("x", "y", "width", "height"))
        except (KeyError, TypeError, ValueError):
            raise ToolError('region must be {"x", "y", "width", "height"} in pixels of the image.') from None
        bbox = (max(0, x), max(0, y), min(width, x + w), min(height, y + h))
        if bbox[2] - bbox[0] < 8 or bbox[3] - bbox[1] < 8:
            raise ToolError(f"region covers less than 8x8 px of the {width}x{height} px image.")
        # A quarter of the smaller side still reaches full strength in the middle; wider would only fade the edit.
        side = min(bbox[2] - bbox[0], bbox[3] - bbox[1])
        fz = min(int(feather), side // 4, REGION_MAX_FEATHER) if feather >= 0 else max(2, min(64, round(0.125 * side)))
    else:
        if feather > 0:
            raise ToolError("feather is for a rectangle (region). A mask is used as painted: for a soft seam, soften "
                            "the mask's own edge (blur it inward, or paint grey toward the edge).")
        fz = 0
        mask = _load_mask(mask_data or b"", (width, height), "mask")
        found = mask.getbbox()
        if not found:
            raise ToolError("The mask selects nothing: paint the area to change white.")
        bbox = found
    bw, bh = bbox[2] - bbox[0], bbox[3] - bbox[1]
    pad = min(int(context), max(width, height)) if context >= 0 else max(48, round(0.35 * max(bw, bh)))
    x0, y0 = max(0, bbox[0] - pad), max(0, bbox[1] - pad)
    x1, y1 = min(width, bbox[2] + pad), min(height, bbox[3] + pad)
    if x1 - x0 < REGION_MIN_BOX:
        x0, x1 = _widen(x0, x1, REGION_MIN_BOX, width)
    if y1 - y0 < REGION_MIN_BOX:
        y0, y1 = _widen(y0, y1, REGION_MIN_BOX, height)
    if x1 - x0 > REGION_MAX_ASPECT * (y1 - y0):
        y0, y1 = _widen(y0, y1, math.ceil((x1 - x0) / REGION_MAX_ASPECT), height)
    elif y1 - y0 > REGION_MAX_ASPECT * (x1 - x0):
        x0, x1 = _widen(x0, x1, math.ceil((y1 - y0) / REGION_MAX_ASPECT), width)
    longest = 2048 // 256  # _snap gives the model 256 to 2048 px a side: a longer box (in a long thin image) would be
    if x1 - x0 > longest * (y1 - y0):  # squeezed, so it gives up context instead
        x0, x1 = _trim(x0, x1, longest * (y1 - y0), bbox[0], bbox[2])
    elif y1 - y0 > longest * (x1 - x0):
        y0, y1 = _trim(y0, y1, longest * (x1 - x0), bbox[1], bbox[3])
    cw, ch = x1 - x0, y1 - y0
    area = cw * ch
    target = REGION_WORK_PIXELS if area < REGION_WORK_PIXELS else min(area, REGION_MAX_PIXELS)
    scale = min(REGION_MAX_UPSCALE, (target / area) ** 0.5, 2048 / max(cw, ch))  # _snap allows 256 to 2048 a side
    work = (_snap(round(cw * scale)), _snap(round(ch * scale)))

    if mask is not None:
        blend = mask.crop((x0, y0, x1, y1))
    else:
        margin = math.ceil(1.5 * fz) + 1 if fz else 0  # room for the blur: three times its sigma
        ox0, oy0 = max(0, x0 - margin), max(0, y0 - margin)
        ox1, oy1 = min(width, x1 + margin), min(height, y1 + margin)
        part = PILImage.new("L", (ox1 - ox0, oy1 - oy0), 0)
        part.paste(255, (bbox[0] - ox0, bbox[1] - oy0, bbox[2] - ox0, bbox[3] - oy0))
        blend = _pad_edges(part, margin - (x0 - ox0), margin - (y0 - oy0), margin - (ox1 - x1), margin - (oy1 - y1))
        if fz:  # 0 at the rectangle's edge, 255 from about one feather inside it
            blend = blend.filter(ImageFilter.GaussianBlur(fz / 2)).point(
                lambda v: max(0, min(255, round((v - 127.5) * 2))))
        blend = blend.crop((margin, margin, margin + cw, margin + ch))
    if protect_data is not None:
        keep = _load_mask(protect_data, (width, height), "protect mask").crop((x0, y0, x1, y1))
        blend = ImageChops.multiply(blend, keep.point(lambda v: 0 if v >= 128 else 255))
    if not blend.getbbox():
        raise ToolError("Nothing is left to change: the protect mask covers the whole area.")
    return {"region": [bbox[0], bbox[1], bw, bh], "context_box": [x0, y0, cw, ch], "work_size": list(work),
            "feather": fz, "blend": blend}


def composite_region(src: PILImage.Image, edited_crop: bytes, plan: dict[str, Any]) -> tuple[bytes, PILImage.Image, dict[str, Any]]:
    """Blend the model's edited context crop into the image through the plan's mask.

    Returns (PNG bytes of the whole image, the context box after the edit, what changed). It checks the result
    instead of trusting the blend: a pixel that changed where the mask is 0 is an error, not a result."""
    x0, y0, cw, ch = plan["context_box"]
    blend: PILImage.Image = plan["blend"]
    before = src.crop((x0, y0, x0 + cw, y0 + ch))
    edited = PILImage.open(io.BytesIO(edited_crop)).convert("RGB").resize((cw, ch), PILImage.LANCZOS)
    if src.mode == "RGBA":  # the model never sees alpha, so the image keeps its own
        edited.putalpha(before.getchannel("A"))
    after = PILImage.composite(edited, before, blend)
    out = src.copy()
    out.paste(after, (x0, y0))
    changed = _any_band(ImageChops.difference(out, src)).point(lambda v: 255 if v else 0)
    allowed = PILImage.new("L", src.size, 0)
    allowed.paste(blend.point(lambda v: 255 if v else 0), (x0, y0))
    if ImageChops.subtract(changed, allowed).getbbox():
        raise ToolError("edit_region changed pixels outside its mask. This is a bug in the server: nothing was saved.")
    box = changed.getbbox()
    buf = io.BytesIO()
    out.save(buf, format="PNG")
    return buf.getvalue(), after, {
        "changed_box": [box[0], box[1], box[2] - box[0], box[3] - box[1]] if box else None,
        "changed_pixels": changed.histogram()[255],
        "outside_mask_identical": True,
    }


def _region_place(region: list[int], box: list[int]) -> str:
    """Where the region lies in the context box, in words for the prompt ("" when it fills most of the box).

    Words, not FLUX.3's bounding-box layout table: in tests on a painted map the table made the model keep the outline
    of a thing it was told to remove and stretch a new thing to fill the box, while words placed it and did neither."""
    rx, ry, rw, rh = region
    if rw > 0.7 * box[2] and rh > 0.7 * box[3]:
        return ""
    fx, fy = (rx + rw / 2 - box[0]) / box[2], (ry + rh / 2 - box[1]) / box[3]
    side = "left" if fx < 0.4 else "right" if fx > 0.6 else ""
    level = "upper" if fy < 0.4 else "lower" if fy > 0.6 else ""
    return f"the {' '.join(w for w in (level, side) if w)} part" if side or level else "the centre"


def build_region_graph(p: dict[str, Any], image: str, box: list[int], work: list[int], prompt: str, negative: str,
                       seed: int, ref_names: list[str], prefix: str) -> dict[str, Any]:
    """FLUX.3 Image Klein reference edit of one crop of an image: crop the context box, scale it to the working
    size, give it to the model as <ref_image_0> and sample a fresh image of that size. The same wiring as ComfyUI's
    image_flux3_klein_image_edit templates, with one ReferenceLatent per reference."""
    g: dict[str, Any] = {
        "unet": {"class_type": "UNETLoader", "inputs": {"unet_name": p["unet"], "weight_dtype": "default"}},
        "clip": {"class_type": "CLIPLoader", "inputs": {"clip_name": p["clip"], "type": "flux3", "device": "default"}},
        "vae": {"class_type": "VAELoader", "inputs": {"vae_name": p["vae"]}},
        "src": {"class_type": "LoadImage", "inputs": {"image": image}},
        "crop": {"class_type": "ImageCrop", "inputs": {"image": ["src", 0], "x": box[0], "y": box[1],
                                                         "width": box[2], "height": box[3]}},
        "ref0": {"class_type": "ImageScale", "inputs": {"image": ["crop", 0], "upscale_method": "lanczos",
                                                         "width": work[0], "height": work[1], "crop": "disabled"}},
        "pos_text": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["clip", 0], "text": prompt}},
        "neg_text": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["clip", 0], "text": negative or ""}},
    }
    for i, name in enumerate(ref_names, start=1):
        g[f"img{i}"] = {"class_type": "LoadImage", "inputs": {"image": name}}
        # About 1 megapixel is plenty for a reference; a 4K one would only cost time and VRAM.
        g[f"ref{i}"] = {"class_type": "ImageScaleToTotalPixels", "inputs": {
            "image": [f"img{i}", 0], "upscale_method": "lanczos", "megapixels": 1.0, "resolution_steps": 16}}
    pos, neg = ["pos_text", 0], ["neg_text", 0]
    for i in range(len(ref_names) + 1):
        g[f"lat{i}"] = {"class_type": "VAEEncode", "inputs": {"pixels": [f"ref{i}", 0], "vae": ["vae", 0]}}
        g[f"pos{i}"] = {"class_type": "ReferenceLatent", "inputs": {"conditioning": pos, "latent": [f"lat{i}", 0]}}
        g[f"neg{i}"] = {"class_type": "ReferenceLatent", "inputs": {"conditioning": neg, "latent": [f"lat{i}", 0]}}
        pos, neg = [f"pos{i}", 0], [f"neg{i}", 0]
    g.update({
        "latent": {"class_type": "EmptyFlux2LatentImage", "inputs": {"width": work[0], "height": work[1], "batch_size": 1}},
        "sampler": {"class_type": "KSampler", "inputs": {
            "model": ["unet", 0], "positive": pos, "negative": neg, "latent_image": ["latent", 0], "seed": seed,
            "steps": p["steps"], "cfg": p["cfg"], "sampler_name": p["sampler"], "scheduler": p["scheduler"], "denoise": 1.0}},
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["sampler", 0], "vae": ["vae", 0]}},
        "save": {"class_type": "SaveImage", "inputs": {"images": ["decode", 0], "filename_prefix": prefix}},
    })
    return g


@mcp.tool(structured_output=False)
async def edit_region(
    prompt: str,
    filename: str = "",
    subfolder: str = "",
    source: str = "output",
    image_base64: str = "",
    region: dict[str, int] | None = None,
    mask: dict[str, str] | None = None,
    protect: dict[str, str] | None = None,
    reference_images: list[dict[str, str]] | None = None,
    context: int = -1,
    feather: int = -1,
    quality: bool = False,
    seed: int = -1,
    negative_prompt: str = "",
    filename_prefix: str = "region",
    preview: bool = True,
    project: str = "",
) -> CallToolResult:
    """Change ONE AREA of an image and keep every other pixel exactly as it is (FLUX.3 Image Klein).

    For local fixes to a large or approved image: move a lake, open a mountain pass, remove a symbol, add a
    building. The server crops a context box around the area, lets the model repaint that crop at about 1 megapixel,
    scales it back and blends it in through the area's mask (a rectangle gets a soft edge, a mask is used as
    painted). Pixels outside the area are the original's, bit for
    bit (the result says so in outside_mask_identical), so an image of any size keeps its resolution and you can
    chain calls: pass each result's filename/subfolder to the next call.

    Args:
        prompt: what to change, written for the crop the model sees (the area plus some surroundings): say what the
            area should become and name what must stay. The server adds "keep everything else" and says roughly
            where in the crop the area is; to write the whole instruction yourself, mention <ref_image_0> (the
            crop) in it. Only what lands inside the area is kept, so ask for one thing that fits it.
        filename, subfolder, source: the image on the server (a result, or an upload_link file with source="input"), or
        image_base64: a small local image instead (PNG/JPEG bytes, base64).
        region: {"x", "y", "width", "height"} in pixels of the image: the rectangle that may change. Give this or
        mask: {"filename", "subfolder", "source"} or {"image_base64"}: an image the same size as the main one, white
            where it may change (black or transparent = keep, grey = blend partly). For shapes a rectangle can't
            give. It is used exactly as painted, with no feather added: a hard mask edge gives a hard seam, so for
            a soft one blur the mask's edge inward or paint grey toward it.
        protect: optional mask image, same form and size: white = never change, even inside the area (a river
            crossing it, a character in front of it). Its edge is hard.
        reference_images: optional, max 2, each {"filename", "subfolder", "source"} or {"image_base64"}: cite them
            in the prompt as <ref_image_1> and <ref_image_2> ("the tower from <ref_image_1>").
        context: surroundings shown to the model on each side of the area, in px (-1 = automatic, about a third of
            the area's size). More context helps it match the surroundings; less gives the area more detail.
        feather: for a region only: width in px of its soft edge (-1 = automatic, an eighth of the smaller side up
            to 64; 0 = hard edge; at most a quarter of the smaller side and 256). It runs INWARD from the
            rectangle's edge, so make the rectangle a little bigger than the thing you change.
        quality: false = FLUX.3 Klein distilled, 16 steps (a few seconds); true = Klein Base, 50 steps (slower,
            more varied, takes a negative prompt).
        seed: -1 for random.
        negative_prompt: things to avoid (only used when quality=true).
        filename_prefix: name stem for saved files (ComfyUI/output/mcp/).
        preview: return a picture of the edited context box.
        project: the game this is for (default: the active project); the result is logged under it.
    The result's first image is the whole image with the edit; the second is the model's raw crop. Images must be
    8 bits a channel (RGB, RGBA, grey or palette); the result is an RGB or RGBA PNG, upright if the file carried a
    rotation tag, and the area's coordinates are those of the upright image.
    """
    use_project(project)
    if (region is None) == (mask is None):
        raise ToolError("Give either region ({x, y, width, height}) or mask (an image, white = change), not both.")
    refs = list(reference_images or [])
    if len(refs) > 2:
        raise ToolError("At most 2 reference_images (<ref_image_1> and <ref_image_2>).")
    src_data, _ = await read_ref(filename=filename, subfolder=subfolder, source=source, data_base64=image_base64)
    mask_data = (await read_ref(mask))[0] if mask is not None else None
    protect_data = (await read_ref(protect))[0] if protect is not None else None
    ref_data = [(await read_ref(r))[0] for r in refs]
    src = await asyncio.to_thread(_load_rgb, src_data, "image")
    plan = await asyncio.to_thread(plan_region, src, region, mask_data, protect_data, context, feather)
    p = await resolve_preset(REGION_PRESETS[bool(quality)])
    image = await input_image_ref(filename, subfolder, source, image_base64)
    ref_names = [await input_image_ref(r.get("filename", ""), r.get("subfolder", ""), r.get("source", "output"),
                                       r.get("image_base64", "")) for r in refs]
    box, work = plan["context_box"], plan["work_size"]
    if "<ref_image_0>" in prompt:
        prompt_used = prompt
    else:
        where = _region_place(plan["region"], box)
        prompt_used = (f"Edit <ref_image_0>: {prompt.strip()} " + (f"Make this change in {where} of the image. " if where else "")
                       + "Keep everything else in <ref_image_0> exactly as it is: the same framing, scale, art style, "
                         "colours and brushwork.")
    negative = negative_prompt if p["cfg"] > 1 else ""
    seed = random.randint(0, 2**50) if seed is None or seed < 0 else seed
    prefix = _safe_name(filename_prefix, "region")
    graph = build_region_graph(p, image, box, work, prompt_used, negative, seed, ref_names, f"mcp/{prefix}_crop")
    prompt_id, entry, elapsed = await queue_and_wait(graph)
    crops = await collect_images(entry, primary_node="none")  # the raw crop is the secondary result
    if not crops:
        raise ToolError(f"ComfyUI finished prompt {prompt_id} without an image.")
    crop_data, _ = await read_ref(filename=crops[0]["filename"], subfolder=crops[0]["subfolder"], source="output")
    png, after, changed = await asyncio.to_thread(composite_region, src, crop_data, plan)
    info = save_output(png, "mcp", prefix, "png")  # on the event loop, like every caller: it picks the next free name

    def hashed(name: str, data: bytes) -> dict[str, str]:
        return {"file": name, "sha256": hashlib.sha256(data).hexdigest()}

    def named(ref: dict[str, str] | None, data: bytes | None) -> dict[str, str] | None:
        if ref is None or data is None:
            return None
        if not ref.get("filename"):
            return hashed("uploaded", data)
        return hashed(f"{ref.get('subfolder', '')}/{ref['filename']} [{ref.get('source', 'output')}]".lstrip("/"), data)

    summary = {
        "prompt_id": prompt_id, "prompt": prompt, "prompt_used": prompt_used, "negative_used": negative or None,
        "seed": seed, "preset": REGION_PRESETS[bool(quality)], "model": p["unet"], "steps": p["steps"], "cfg": p["cfg"],
        "image_size": list(src.size), "region": plan["region"], "context_box": box, "work_size": work,
        "feather": plan["feather"], **changed,
        "inputs": {"image": hashed(image, src_data), "mask": named(mask, mask_data),
                   "protect": named(protect, protect_data),
                   "references": [hashed(n, d) for n, d in zip(ref_names, ref_data)]},
        "seconds": round(elapsed, 1),
    }
    if negative_prompt and not negative:
        summary["negative_ignored"] = negative_prompt  # the distilled model runs at CFG 1
    notes = []
    if work[0] * work[1] < 0.9 * box[2] * box[3]:
        notes.append("The context box is bigger than the model's working size, so the edited area is softer than the "
                     "rest of the image. Use a smaller region, or less context, to keep full detail.")
    if 0 <= plan["feather"] < feather:
        notes.append(f"feather was cut from {feather} to {plan['feather']} px: it is at most a quarter of the "
                     f"rectangle's smaller side and {REGION_MAX_FEATHER} px, so the middle changes fully.")
    if notes:
        summary["note"] = " ".join(notes)
    result = await build_result(summary, [file_entry(info, "images"), *crops], preview=False, tool="edit_region")
    if preview:
        result.content.append(png_preview(after))
    return result


def _prune_to(graph: dict[str, Any], keep_outputs: list[str]) -> dict[str, Any]:
    """Keep only nodes the given output nodes depend on."""
    keep: set[str] = set()
    stack = list(keep_outputs)
    while stack:
        nid = stack.pop()
        if nid in keep or nid not in graph:
            continue
        keep.add(nid)
        for v in graph[nid]["inputs"].values():
            if isinstance(v, list) and len(v) == 2 and isinstance(v[0], str):
                stack.append(v[0])
    return {k: v for k, v in graph.items() if k in keep}


def load_3d_workflow() -> dict[str, Any]:
    if not THREE_D_WORKFLOW:
        raise ToolError("Set COMFY_3D_WORKFLOW to a compatible ComfyUI API-format 3D workflow JSON.")
    try:
        graph = json.loads(Path(THREE_D_WORKFLOW).expanduser().read_text(encoding="utf-8-sig"))
        if not isinstance(graph, dict) or any(
            not isinstance(node, dict) or not isinstance(node.get("inputs"), dict) or "class_type" not in node
            for node in graph.values()
        ) or not any(
            node["class_type"] == "Save3DAdvanced" and "viewport_state" in node["inputs"]
            for node in graph.values()
        ):
            raise ValueError("invalid API-format workflow")
    except (OSError, ValueError):
        raise ToolError("COMFY_3D_WORKFLOW must be readable API-format JSON with a Save3DAdvanced viewport.") from None
    return graph


async def _hunyuan3d_to_glb(annotated: str, face_count: int, remove_background: bool, seed: int, filename_prefix: str,
                            uv_resolution: int, viewport_state: Any) -> CallToolResult:
    """Hunyuan3D 2.1 shape generation. Mirrors ComfyUI's 3d_hunyuan3d-v2.1 template, plus cut-out, decimation,
    smooth normals and a UV atlas."""
    lists = await model_lists()
    ckpt = _resolve(HUNYUAN3D_CKPT, lists["checkpoints"])
    if ckpt is None:
        raise ToolError(f"Hunyuan3D model missing on the server: {HUNYUAN3D_CKPT}")
    seed = random.randint(0, 2**50) if seed is None or seed < 0 else seed
    safe_prefix = _safe_name(filename_prefix, "model3d")
    g: dict[str, Any] = {
        "ckpt": {"class_type": "ImageOnlyCheckpointLoader", "inputs": {"ckpt_name": ckpt}},
        "load": {"class_type": "LoadImage", "inputs": {"image": annotated}},
    }
    img_ref: list[Any] = ["load", 0]
    if remove_background:
        g["bg_model"] = {"class_type": "LoadBackgroundRemovalModel", "inputs": {"bg_removal_name": BG_REMOVAL_MODEL}}
        g["bg_mask"] = {"class_type": "RemoveBackground", "inputs": {"bg_removal_model": ["bg_model", 0], "image": ["load", 0]}}
        g["crop"] = {"class_type": "ImageCropToMask", "inputs": {"images": ["load", 0], "masks": ["bg_mask", 0], "width": 1024,
                                                               "height": 1024, "pad_factor": 1.1, "grow_mask": 0, "background": "#FFFFFF"}}
        img_ref = ["crop", 0]
    g.update({
        "shift": {"class_type": "ModelSamplingAuraFlow", "inputs": {"model": ["ckpt", 0], "shift": 1.0}},
        "clipenc": {"class_type": "CLIPVisionEncode", "inputs": {"clip_vision": ["ckpt", 1], "image": img_ref, "crop": "center"}},
        "cond": {"class_type": "Hunyuan3Dv2Conditioning", "inputs": {"clip_vision_output": ["clipenc", 0]}},
        "latent": {"class_type": "EmptyLatentHunyuan3Dv2", "inputs": {"resolution": 4096, "batch_size": 1}},
        "sampler": {"class_type": "KSampler", "inputs": {
            "model": ["shift", 0], "positive": ["cond", 0], "negative": ["cond", 1], "latent_image": ["latent", 0],
            "seed": seed, "steps": 30, "cfg": 5.0, "sampler_name": "euler", "scheduler": "normal", "denoise": 1.0}},
        "vaedec": {"class_type": "VAEDecodeHunyuan3D", "inputs": {"samples": ["sampler", 0], "vae": ["ckpt", 2],
                                                                 "num_chunks": 8000, "octree_resolution": 256}},
        "mesh": {"class_type": "VoxelToMesh", "inputs": {"voxel": ["vaedec", 0], "algorithm": "surface net", "threshold": 0.6}},
        "decimate": {"class_type": "DecimateMesh", "inputs": {"mesh": ["mesh", 0], "target_face_count": max(1000, int(face_count)),
                                                             "placement_mode": "midpoint"}},
        # Same post-processing nodes the TRELLIS.2 template uses: smooth normals + UV atlas, so the mesh is ready to texture
        "normals": {"class_type": "MeshSmoothNormals", "inputs": {"mesh": ["decimate", 0], "crease_angle": 180}},
        "unwrap": {"class_type": "UnwrapMesh", "inputs": {"mesh": ["normals", 0], "segmenter": "pec", "resolution": int(uv_resolution),
                                                         "padding": 1, "weld_distance": 0.0002}},
        "file3d": {"class_type": "MeshToFile3D", "inputs": {"mesh": ["unwrap", 0]}},
        "save": {"class_type": "Save3DAdvanced", "inputs": {"model_3d": ["file3d", 0], "filename_prefix": f"mcp/3d/{safe_prefix}",
                                                           "viewport_state": viewport_state, "width": 1024, "height": 1024}},
    })
    prompt_id, entry, elapsed = await queue_and_wait(g, THREE_D_TIMEOUT_S)
    files = await collect_images(entry, primary_node="save")
    summary = {"prompt_id": prompt_id, "source_image": annotated, "model": "hunyuan3d", "seed": seed, "face_count": face_count,
               "textured": False, "seconds": round(elapsed, 1),
               "note": "Hunyuan3D 2.1 output is untextured geometry with smooth normals and a UV atlas (ready to texture)."}
    return await build_result(summary, files, preview=False, tool="image_to_3d")


@mcp.tool(structured_output=False)
async def image_to_3d(
    filename: str = "",
    subfolder: str = "",
    source: str = "output",
    image_base64: str = "",
    face_count: int = 50000,
    texture_size: int = 2048,
    remove_background: bool = True,
    model: str = "pixal3d",
    seed: int = -1,
    filename_prefix: str = "model3d",
    project: str = "",
) -> CallToolResult:
    """Turn one image of an object/character into a textured 3D model (.glb) with UVs and PBR textures.

    Requires COMFY_3D_WORKFLOW and installed models. The Pixal3D + TRELLIS.2 pipeline can cut out the subject, estimate
    the camera with MoGe, generate the shape, remesh, decimate, UV-unwrap, then bake base color / metallic /
    roughness, a normal map and ambient occlusion. Output is a GLB you can import into Unreal, Unity or Blender.

    Best input: a single object or character, centred, full body/object visible, plain background. A good recipe
    is generate_image(..., transparent=true) first, then pass that result's filename/subfolder here.

    Args:
        filename, subfolder, source: an image already on the server (e.g. from generate_image: source "output").
        image_base64: or upload a local image instead (PNG/JPEG bytes, base64).
        face_count: target triangle count after decimation (mobile ~5k-20k, PC game ~30k-100k; template default 700000).
        texture_size: baked texture resolution (1024, 2048 or 4096).
        remove_background: cut the subject out first (keep true unless the image is already transparent and clean).
        model: "pixal3d" (default, uses estimated camera; best for most images), "trellis2" (Microsoft's original),
            or "hunyuan3d" (Tencent Hunyuan3D 2.1: fast, often crisper SHAPE, but UNTEXTURED geometry with normals and
            UVs; texture it elsewhere, or use it as a sculpt/blockout base). For hunyuan3d, texture_size sets the UV atlas size.
        seed: -1 for random.
    Takes a few minutes. Returns the GLB download URL(s).
    project: the game this result is logged under (default: the active project).
    """
    use_project(project)
    if model not in ("pixal3d", "trellis2", "hunyuan3d"):
        raise ToolError("model must be 'pixal3d', 'trellis2' or 'hunyuan3d'")
    g = load_3d_workflow()
    save_id = next(k for k, v in g.items() if v["class_type"] == "Save3DAdvanced" and "viewport_state" in v["inputs"])
    annotated = await input_image_ref(filename, subfolder, source, image_base64)
    if model == "hunyuan3d":
        return await _hunyuan3d_to_glb(annotated, face_count, remove_background, seed, filename_prefix, texture_size,
                                      g[save_id]["inputs"]["viewport_state"])
    g = _prune_to(g, [save_id])
    seed = random.randint(0, 2**50) if seed is None or seed < 0 else seed
    safe_prefix = "".join(c for c in filename_prefix if c.isalnum() or c in "-_") or "model3d"
    for i, (nid, node) in enumerate(sorted((k, v) for k, v in g.items() if v["class_type"] == "KSampler")):
        node["inputs"]["seed"] = seed + i
    for node in g.values():
        ct, inp = node["class_type"], node["inputs"]
        if ct == "LoadImage":
            inp["image"] = annotated
        elif ct == "DecimateMesh":
            inp["target_face_count"] = max(1000, int(face_count))
        elif ct == "PrimitiveInt" and node.get("_meta", {}).get("title") == "Texture Resolution":
            inp["value"] = int(texture_size)
        elif ct == "BakeNormalMapFromMesh":
            inp["resolution"] = int(texture_size)
        elif ct == "PrimitiveBoolean" and "Trellis2" in node.get("_meta", {}).get("title", ""):
            inp["value"] = model == "trellis2"
        elif ct == "ComfySwitchNode" and "Remove background" in node.get("_meta", {}).get("title", ""):
            inp["switch"] = bool(remove_background)
    g[save_id]["inputs"]["filename_prefix"] = f"mcp/3d/{safe_prefix}"
    prompt_id, entry, elapsed = await queue_and_wait(g, THREE_D_TIMEOUT_S)
    files = await collect_images(entry, primary_node=save_id)
    summary = {
        "prompt_id": prompt_id, "source_image": annotated, "model": model, "seed": seed, "face_count": face_count,
        "texture_size": texture_size, "seconds": round(elapsed, 1),
    }
    return await build_result(summary, files, preview=False)


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True), structured_output=False)
async def server_status() -> str:
    """Show whether ComfyUI is running, whether models are loaded, idle time, and when they will auto-unload."""
    running = await comfy_up()
    out: dict[str, Any] = {"comfyui_running": running, **lifecycle_status()}
    if running:
        q = (await _http.get("/queue", timeout=5)).json()
        out["queue"] = {"running": len(q.get("queue_running", [])), "pending": len(q.get("queue_pending", []))}
        out.update(await gpu_info())
    else:
        out["models_loaded"] = False
        out["note"] = "ComfyUI is unavailable. Start the backend separately."
    if out["gaming_mode"]:
        out["note"] = PAUSED_MSG
    return json.dumps(out, indent=2)


@mcp.tool(structured_output=False)
async def load_models(preset: str = "z_image_turbo") -> str:
    """Load a preset's models into VRAM so the next generate_image call can reuse them.

    Not required: generate_image loads models on demand. Useful before a batch, or to pay the cold-load cost up front.
    Models stay loaded until the idle timeout (see server_status) or unload_models.
    """
    await ensure_comfy()
    p = await resolve_preset(preset)
    g = build_graph(p, "warm-up", None, 256, 256, random.randint(0, 2**50), 1, p["cfg"], 1, False, "mcp/warmup")
    g["save"] = {"class_type": "PreviewImage", "inputs": {"images": ["decode", 0]}}  # don't write a file
    _, _, elapsed = await queue_and_wait(g)
    return json.dumps({"loaded_preset": preset, "seconds": round(elapsed, 1), **lifecycle_status(), **(await gpu_info())}, indent=2)


@mcp.tool(structured_output=False)
async def unload_models() -> str:
    """Unload all models from VRAM and system RAM right now (ComfyUI keeps running). Next job reloads them."""
    if not await comfy_up():
        _state["models_loaded"] = False
        return json.dumps({"unloaded": True, "note": "ComfyUI is not running; nothing is loaded."})
    info = await unload("requested")
    return json.dumps({"unloaded": True, **info}, indent=2)


@mcp.tool(structured_output=False)
async def run_workflow(workflow: dict[str, Any], timeout_s: int = DEFAULT_TIMEOUT_S, preview: bool = True, project: str = "") -> CallToolResult:
    """Queue an arbitrary ComfyUI workflow in API format ({node_id: {class_type, inputs}}) and wait for it.

    Use list_models for exact model filenames (names inside subfolders include the folder, e.g. 'sub\\file.safetensors').
    Returns every saved image with its download URL.
    project: the game this result is logged under (default: the active project).
    """
    use_project(project)
    if "nodes" in workflow and "links" in workflow:
        raise ToolError("That is a UI-format workflow. Export it with 'Export (API)' in ComfyUI and pass that JSON.")
    prompt_id, entry, elapsed = await queue_and_wait(workflow, timeout_s)
    images = await collect_images(entry)
    return await build_result({"prompt_id": prompt_id, "seconds": round(elapsed, 1)}, images, preview)


# ---------------------------------------------------------------------------
# Asset finishing tools (CPU only; no GPU / ComfyUI queue involved)
# ---------------------------------------------------------------------------
def _safe_name(s: str, default: str) -> str:
    return "".join(c for c in s if c.isalnum() or c in "-_") or default


async def read_ref(ref: dict[str, Any] | None = None, *, filename: str = "", subfolder: str = "", source: str = "output",
                   data_base64: str = "", max_bytes: int = 0) -> tuple[bytes, str]:
    """Bytes of a server file (ComfyUI output/input) or of base64 data. Returns (data, display_name).
    max_bytes refuses a bigger server file before it is read."""
    if ref:
        filename = ref.get("filename", "")
        subfolder = ref.get("subfolder", "")
        source = ref.get("source", "output")
        data_base64 = ref.get("image_base64", "") or ref.get("data_base64", "")
    if data_base64:
        try:
            return base64.b64decode(data_base64.split(",", 1)[-1]), "uploaded"
        except ValueError:
            raise ToolError("The base64 data could not be decoded.") from None
    if not filename:
        raise ToolError("Provide filename (+subfolder/source) or base64 data.")
    base = COMFY_DIRS.get(source)
    if base is None:
        raise ToolError("source must be 'output' or 'input'")
    path = (base / subfolder.replace("\\", "/") / filename).resolve()
    if not path.is_relative_to(base.resolve()) or not path.is_file():
        raise ToolError(f"File not found on the server: {source}/{subfolder}/{filename}")
    if max_bytes and path.stat().st_size > max_bytes:
        raise ToolError(f"{filename} is {path.stat().st_size / 2**20:.1f} MB; at most {max_bytes // 2**20} MB here.")
    return path.read_bytes(), f"{subfolder}/{filename}".strip("/")


def save_output(data: bytes, subfolder: str, prefix: str, ext: str, exact: bool = False) -> dict[str, Any]:
    """Write into ComfyUI's output folder (so signed links can serve it) as <prefix>_00001.<ext> (or <prefix>.<ext>)."""
    folder = COMFY_DIRS["output"] / subfolder
    folder.mkdir(parents=True, exist_ok=True)
    if exact:
        name = f"{prefix}.{ext}"
    else:
        n = 1
        while (folder / f"{prefix}_{n:05d}.{ext}").exists():
            n += 1
        name = f"{prefix}_{n:05d}.{ext}"
    (folder / name).write_bytes(data)
    return {"filename": name, "subfolder": subfolder, "type": "output"}


def file_entry(info: dict[str, Any], kind: str, role: str = "primary") -> dict[str, Any]:
    return {**info, "kind": kind, "role": role, "url": signed_url(info),
            "urls": {name: signed_url(info, base) for name, base in PUBLIC_URLS.items()}}


def png_preview(im: PILImage.Image, max_side: int = 512) -> ImageContent:
    im = im.copy()
    im.thumbnail((max_side, max_side))
    buf = io.BytesIO()
    im.save(buf, format="PNG", optimize=True)
    return ImageContent(type="image", data=base64.b64encode(buf.getvalue()).decode(), mime_type="image/png")


def glb_stats(data: bytes) -> dict[str, Any]:
    """Triangles / vertices / textures of a GLB (binary glTF)."""
    import struct
    if data[:4] != b"glTF":
        raise ToolError("Not a GLB (binary glTF) file.")
    off, chunks = 12, []
    while off + 8 <= len(data):
        clen, ctype = struct.unpack_from("<I4s", data, off)
        chunks.append(data[off + 8: off + 8 + clen])
        off += 8 + clen
    g = json.loads(chunks[0])
    binb = chunks[1] if len(chunks) > 1 else b""
    acc = g.get("accessors", [])
    tris = verts = 0
    for m in g.get("meshes", []):
        for p in m.get("primitives", []):
            if "indices" in p:
                tris += acc[p["indices"]]["count"] // 3
            elif "POSITION" in p.get("attributes", {}):
                tris += acc[p["attributes"]["POSITION"]]["count"] // 3
            if "POSITION" in p.get("attributes", {}):
                verts += acc[p["attributes"]["POSITION"]]["count"]
    textures = []
    for img in g.get("images", []):
        if "bufferView" in img:
            v = g["bufferViews"][img["bufferView"]]
            raw = binb[v.get("byteOffset", 0): v.get("byteOffset", 0) + v["byteLength"]]
            try:
                w, h = PILImage.open(io.BytesIO(raw)).size
            except Exception:  # noqa: BLE001
                w = h = 0
            textures.append(f"{w}x{h} {img.get('mimeType', '?')}")
    return {"triangles": tris, "vertices": verts, "size_mb": round(len(data) / 1e6, 2), "textures": textures,
            "extensions_required": g.get("extensionsRequired", [])}


@mcp.tool(structured_output=False)
async def optimize_glb(
    filename: str = "",
    subfolder: str = "mcp/3d",
    source: str = "output",
    glb_base64: str = "",
    target_faces: int = 0,
    texture_size: int = 1024,
    texture_format: str = "keep",
    max_error: float = 0.01,
    filename_prefix: str = "",
    project: str = "",
) -> CallToolResult:
    """Make a GLB game-ready: simplify the mesh, shrink textures, clean up (gltf-transform + meshoptimizer).

    Typical use: after image_to_3d, make a mobile version, e.g. target_faces=8000, texture_size=1024.
    Geometry is NOT Draco/meshopt-compressed, so Godot, Unity and Unreal all import it.

    Args:
        filename, subfolder, source: a GLB on the server (image_to_3d saves to subfolder "mcp/3d"; pass the
            exact subfolder from its result), or glb_base64 to upload one.
        target_faces: triangle target (0 = keep geometry; mobile ~2k-20k, PC ~30k-100k). Simplification also stops
            early if it would exceed max_error, so the shape is protected.
        texture_size: maximum texture width/height in px (e.g. 512, 1024, 2048; 0 = keep).
        texture_format: "keep" (same format, e.g. PNG) or "webp" (much smaller; Godot 4 and Blender read it,
            some older importers don't).
        max_error: simplification error limit as a fraction of the model size (0.01 = 1%).
    Returns before/after stats and a download link to the optimised GLB.
    project: the game this result is logged under (default: the active project).
    """
    use_project(project)
    if texture_format not in ("keep", "webp"):
        raise ToolError("texture_format must be 'keep' or 'webp'")
    cli = Path(GLTF_TRANSFORM_CLI).expanduser() if GLTF_TRANSFORM_CLI else None
    node = shutil.which("node")
    if cli is None or not cli.is_file() or node is None:
        raise ToolError("Install Node.js and set GLTF_TRANSFORM_CLI to the installed gltf-transform cli.js.")
    data, name = await read_ref(filename=filename, subfolder=subfolder, source=source, data_base64=glb_base64)
    before = glb_stats(data)
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        src, dst = Path(td) / "in.glb", Path(td) / "out.glb"
        src.write_bytes(data)
        args = [node, str(cli), "optimize", str(src), str(dst), "--compress", "false",
                "--texture-compress", "webp" if texture_format == "webp" else "false",
                "--palette", "false", "--instance", "false",
                # gltf-transform defaults to 2048 max; 0 means "keep" here
                "--texture-size", str(int(texture_size)) if texture_size and texture_size > 0 else "16384"]
        if target_faces and target_faces > 0 and before["triangles"] > target_faces:
            ratio = max(0.001, min(1.0, target_faces / before["triangles"]))
            args += ["--simplify", "true", "--simplify-ratio", f"{ratio:.4f}", "--simplify-error", str(max_error)]
        else:
            args += ["--simplify", "false"]
        steps = [args]
        if texture_format == "keep" and texture_size and texture_size > 0:
            # optimize only resizes inside its texture-compress step, so resize separately when keeping the format
            resized = Path(td) / "resized.glb"
            steps.append([node, str(cli), "resize", str(dst), str(resized),
                          "--width", str(int(texture_size)), "--height", str(int(texture_size))])
        for step in steps:
            proc = await asyncio.create_subprocess_exec(*step, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT)
            out, _ = await asyncio.wait_for(proc.communicate(), timeout=600)
            if proc.returncode != 0 or not Path(step[4]).exists():
                raise ToolError(f"gltf-transform {step[2]} failed: {out.decode(errors='replace')[-1500:]}")
        result = Path(steps[-1][4]).read_bytes()
    after = glb_stats(result)
    stem = _safe_name(filename_prefix or Path(name).stem, "model") + "_opt"
    info = save_output(result, "mcp/3d", stem, "glb")
    summary = {"source": name, "before": before, "after": after, "target_faces": target_faces,
               "texture_size": texture_size, "texture_format": texture_format, "files": [file_entry(info, "3d")],
               "download_hint": 'curl -o <file>.glb "<url>"'}
    log_asset("optimize_glb", summary, [info])
    return CallToolResult(content=[TextContent(type="text", text=json.dumps(summary, indent=2))])


def _tres_spriteframes(res_path: str, cells: list[dict[str, Any]], animations: list[dict[str, Any]]) -> str:
    """Godot 4 SpriteFrames resource whose frames are AtlasTextures cut from one sheet."""
    lines = [f'[gd_resource type="SpriteFrames" load_steps={len(cells) + 2} format=3]', "",
             f'[ext_resource type="Texture2D" path="{res_path}" id="1_sheet"]', ""]
    for i, c in enumerate(cells):
        lines += [f'[sub_resource type="AtlasTexture" id="AtlasTexture_{i}"]', 'atlas = ExtResource("1_sheet")',
                  f"region = Rect2({c['x']}, {c['y']}, {c['w']}, {c['h']})", ""]
    anims = []
    for a in animations:
        frames = ",\n".join('{\n"duration": 1.0,\n"texture": SubResource("AtlasTexture_%d")\n}' % i for i in a["cells"])
        anims.append('{\n"frames": [%s],\n"loop": %s,\n"name": &"%s",\n"speed": %s\n}'
                     % (frames, "true" if a["loop"] else "false", a["name"], float(a["fps"])))
    lines += ["[resource]", "animations = [" + ", ".join(anims) + "]", ""]
    return "\n".join(lines)


@mcp.tool(structured_output=False)
async def pack_spritesheet(
    animations: list[dict[str, Any]],
    columns: int = 0,
    padding: int = 2,
    trim: bool = True,
    cell_size: int = 0,
    pixel_art: bool = False,
    res_path: str = "",
    filename_prefix: str = "sheet",
    project: str = "",
) -> CallToolResult:
    """Pack animation frames into one sprite sheet PNG + a ready-to-use Godot 4 SpriteFrames (.tres) + JSON frame data.

    Args:
        animations: list of {"name": "walk", "frames": [<image ref>, ...], "fps": 8, "loop": true}. Each image ref is
            {"filename", "subfolder", "source"} for files on the server (e.g. generate_image / edit_image results)
            or {"image_base64": "..."}. Use transparent PNGs for sprites.
        columns: frames per row (0 = automatic, roughly square sheet).
        padding: transparent pixels between cells (avoids texture bleeding).
        trim: crop the empty border shared by ALL frames (keeps frames aligned to each other).
        cell_size: scale frames to fit an N x N box (e.g. 128 or 256; 0 = keep size). Same scale for every frame.
        pixel_art: use nearest-neighbour scaling (crisp pixels).
        res_path: where the PNG will live in the Godot project, used inside the .tres
            (e.g. "res://assets/characters/knight_sheet.png"; default res://<sheet file name>).
    In Godot: put the PNG at res_path, add the .tres anywhere, assign it to an AnimatedSprite2D's sprite_frames.
    The JSON lists every frame rect, for other engines.
    project: the game this result is logged under (default: the active project).
    """
    use_project(project)
    if not animations:
        raise ToolError("Give at least one animation with frames.")
    frames: list[PILImage.Image] = []
    anim_specs = []
    for a in animations:
        refs = a.get("frames") or []
        if not refs:
            raise ToolError(f"Animation {a.get('name')!r} has no frames.")
        idx = []
        for ref in refs:
            data, _ = await read_ref(ref)
            frames.append(PILImage.open(io.BytesIO(data)).convert("RGBA"))
            idx.append(len(frames) - 1)
        anim_specs.append({"name": _safe_name(str(a.get("name", "default")), "default"), "fps": float(a.get("fps", 8)),
                           "loop": bool(a.get("loop", True)), "cells": idx})
    w = max(f.width for f in frames)
    h = max(f.height for f in frames)
    canvas = []
    for f in frames:  # centre smaller frames on a common canvas
        c = PILImage.new("RGBA", (w, h), (0, 0, 0, 0))
        c.alpha_composite(f, ((w - f.width) // 2, (h - f.height) // 2))
        canvas.append(c)
    if trim:
        boxes = [c.getchannel("A").point(lambda v: 255 if v > 8 else 0).getbbox() for c in canvas]
        boxes = [b for b in boxes if b]
        if boxes:
            box = (min(b[0] for b in boxes), min(b[1] for b in boxes), max(b[2] for b in boxes), max(b[3] for b in boxes))
            canvas = [c.crop(box) for c in canvas]
    fw, fh = canvas[0].size
    if cell_size and cell_size > 0:
        s = cell_size / max(fw, fh)
        nw, nh = max(1, round(fw * s)), max(1, round(fh * s))
        res = PILImage.NEAREST if pixel_art else PILImage.LANCZOS
        canvas = [c.resize((nw, nh), res) for c in canvas]
        fw, fh = nw, nh
    n = len(canvas)
    cols = columns if columns and columns > 0 else max(1, int(np_ceil_sqrt(n)))
    rows = (n + cols - 1) // cols
    pad = max(0, int(padding))
    sheet = PILImage.new("RGBA", (pad + cols * (fw + pad), pad + rows * (fh + pad)), (0, 0, 0, 0))
    cells = []
    for i, c in enumerate(canvas):
        x, y = pad + (i % cols) * (fw + pad), pad + (i // cols) * (fh + pad)
        sheet.alpha_composite(c, (x, y))
        cells.append({"x": x, "y": y, "w": fw, "h": fh})
    prefix = _safe_name(filename_prefix, "sheet")
    buf = io.BytesIO()
    sheet.save(buf, format="PNG", optimize=True)
    png_info = save_output(buf.getvalue(), "mcp/sheets", prefix, "png")
    stem = png_info["filename"].rsplit(".", 1)[0]
    rp = res_path or f"res://{png_info['filename']}"
    tres = _tres_spriteframes(rp, cells, anim_specs)
    tres_info = save_output(tres.encode(), "mcp/sheets", stem, "tres", exact=True)
    meta = {"sheet": png_info["filename"], "size": list(sheet.size), "frame_size": [fw, fh], "columns": cols,
            "padding": pad, "animations": [{"name": a["name"], "fps": a["fps"], "loop": a["loop"],
                                            "frames": [cells[i] for i in a["cells"]]} for a in anim_specs]}
    json_info = save_output(json.dumps(meta, indent=2).encode(), "mcp/sheets", stem, "json", exact=True)
    summary = {"frames": n, "frame_size": [fw, fh], "sheet_size": list(sheet.size), "columns": cols,
               "godot_res_path": rp, "animations": [{k: a[k] for k in ("name", "fps", "loop")} | {"frames": len(a["cells"])}
                                                     for a in anim_specs],
               "files": [file_entry(png_info, "sprite_sheet"), file_entry(tres_info, "godot_spriteframes"),
                         file_entry(json_info, "frame_data")],
               "godot_hint": "Save the PNG at godot_res_path and the .tres anywhere in the project; set an AnimatedSprite2D's sprite_frames to the .tres."}
    summary["project"], summary["project_context_version"] = log_asset(
        "pack_spritesheet", {**summary, "frame_sources": [[r.get("subfolder", ""), r.get("filename", "uploaded")]
                                                          for a in animations for r in (a.get("frames") or [])]},
        [png_info, tres_info, json_info])
    return CallToolResult(content=[TextContent(type="text", text=json.dumps(summary, indent=2)), png_preview(sheet)])


def np_ceil_sqrt(n: int) -> int:
    r = int(n ** 0.5)
    return r if r * r >= n else r + 1


ICON_SETS: dict[str, list[tuple[str, int, str]]] = {
    # (path in zip, size, mode) mode: "opaque" fills the background, "alpha" keeps transparency,
    # "adaptive_fg" = icon inside Android's adaptive safe zone, "adaptive_bg" = solid background, "mono" = white silhouette
    "ios": [(f"ios/AppIcon-{s}.png", s, "opaque") for s in (1024, 180, 167, 152, 120, 87, 80, 76, 60, 58, 40, 29, 20)],
    "android": [(f"android/mipmap-{d}/ic_launcher.png", s, "alpha") for d, s in
                (("mdpi", 48), ("hdpi", 72), ("xhdpi", 96), ("xxhdpi", 144), ("xxxhdpi", 192))]
               + [("android/play_store_512.png", 512, "opaque"), ("android/adaptive_foreground_432.png", 432, "adaptive_fg"),
                  ("android/adaptive_background_432.png", 432, "adaptive_bg"), ("android/adaptive_monochrome_432.png", 432, "mono")],
    "godot": [("godot/icon.png", 512, "alpha"), ("godot/android_main_192x192.png", 192, "alpha"),
              ("godot/android_adaptive_foreground_432x432.png", 432, "adaptive_fg"),
              ("godot/android_adaptive_background_432x432.png", 432, "adaptive_bg"),
              ("godot/android_adaptive_monochrome_432x432.png", 432, "mono"),
              ("godot/ios_icon_1024x1024.png", 1024, "opaque")],
    "web": [("web/favicon-32.png", 32, "alpha"), ("web/apple-touch-icon-180.png", 180, "opaque"),
            ("web/icon-192.png", 192, "alpha"), ("web/icon-512.png", 512, "alpha")],
    "windows": [],  # .ico written separately
}


@mcp.tool(structured_output=False)
async def export_icons(
    filename: str = "",
    subfolder: str = "",
    source: str = "output",
    image_base64: str = "",
    platforms: list[str] | None = None,
    background: str = "#FFFFFF",
    padding: float = 0.08,
    trim: bool = True,
    pixel_art: bool = False,
    filename_prefix: str = "app_icon",
    project: str = "",
) -> CallToolResult:
    """Turn one image into complete app-icon sets (zip) for iOS, Android, Godot export, web and Windows.

    Args:
        filename, subfolder, source / image_base64: the source image. Best: a square-ish, transparent icon
            (e.g. generate_image(..., transparent=true)), at least 1024 px.
        platforms: any of "ios", "android", "godot", "web", "windows" (default: all).
            - ios: AppIcon 20-1024 px, opaque (Apple rejects transparency).
            - android: mipmap ic_launcher 48-192, Play Store 512, adaptive foreground/background/monochrome 432.
            - godot: the exact files Godot 4's export dialog asks for (project icon, Android main/adaptive, iOS 1024).
            - web: favicon + apple-touch + PWA 192/512. windows: icon.ico (16-256).
        background: hex colour used where a platform needs an opaque icon or adaptive background layer.
        padding: empty margin around the subject as a fraction of the icon size (0-0.3).
        trim: crop transparent borders first so the subject is centred and fills the icon.
        pixel_art: nearest-neighbour scaling for crisp pixel icons.
    Returns a zip download plus a preview.
    project: the game this result is logged under (default: the active project).
    """
    use_project(project)
    import zipfile
    plats = [p.lower() for p in (platforms or list(ICON_SETS))]
    bad = [p for p in plats if p not in ICON_SETS]
    if bad:
        raise ToolError(f"Unknown platform(s) {bad}; choose from {list(ICON_SETS)}")
    try:
        bg = PILImage.new("RGBA", (1, 1), background).getpixel((0, 0))
    except ValueError:
        raise ToolError(f"background must be a colour like '#1E90FF', got {background!r}") from None
    data, name = await read_ref(filename=filename, subfolder=subfolder, source=source, data_base64=image_base64)
    src = PILImage.open(io.BytesIO(data)).convert("RGBA")
    if trim:
        box = src.getchannel("A").point(lambda v: 255 if v > 8 else 0).getbbox()
        if box:
            src = src.crop(box)
    side = max(src.size)
    sq = PILImage.new("RGBA", (side, side), (0, 0, 0, 0))
    sq.alpha_composite(src, ((side - src.width) // 2, (side - src.height) // 2))
    resample = PILImage.NEAREST if pixel_art else PILImage.LANCZOS
    pad = min(max(padding, 0.0), 0.3)

    def render(size: int, mode: str) -> PILImage.Image:
        if mode == "adaptive_bg":
            return PILImage.new("RGBA", (size, size), bg[:3] + (255,))
        inner = size * (0.61 if mode in ("adaptive_fg", "mono") else (1 - 2 * pad))  # Android safe zone = 66/108 dp
        s = max(1, round(inner))
        icon = sq.resize((s, s), resample)
        canvas = PILImage.new("RGBA", (size, size), bg[:3] + (255,) if mode == "opaque" else (0, 0, 0, 0))
        canvas.alpha_composite(icon, ((size - s) // 2, (size - s) // 2))
        if mode == "mono":
            alpha = canvas.getchannel("A")
            canvas = PILImage.new("RGBA", (size, size), (255, 255, 255, 0))
            canvas.putalpha(alpha)
        return canvas

    zbuf = io.BytesIO()
    written = []
    with zipfile.ZipFile(zbuf, "w", zipfile.ZIP_DEFLATED) as z:
        for p in plats:
            for path, size, mode in ICON_SETS[p]:
                b = io.BytesIO()
                im = render(size, mode)
                (im.convert("RGB") if mode in ("opaque", "adaptive_bg") else im).save(b, format="PNG", optimize=True)
                z.writestr(path, b.getvalue())
                written.append(path)
            if p == "windows":
                b = io.BytesIO()
                render(256, "alpha").save(b, format="ICO", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
                z.writestr("windows/icon.ico", b.getvalue())
                written.append("windows/icon.ico")
            if p == "web":
                b = io.BytesIO()
                render(48, "alpha").save(b, format="ICO", sizes=[(16, 16), (32, 32), (48, 48)])
                z.writestr("web/favicon.ico", b.getvalue())
                written.append("web/favicon.ico")
        z.writestr("manifest.json", json.dumps({"source": name, "background": background, "padding": pad, "files": written}, indent=2))
    zip_info = save_output(zbuf.getvalue(), "mcp/icons", _safe_name(filename_prefix, "app_icon"), "zip")
    preview = PILImage.new("RGBA", (3 * 256 + 40, 256 + 20), (235, 235, 235, 255))
    for i, mode in enumerate(("opaque", "alpha", "adaptive_fg")):
        preview.alpha_composite(render(256, mode), (10 + i * 266, 10))
    summary = {"source": name, "platforms": plats, "file_count": len(written), "files_in_zip": written,
               "files": [file_entry(zip_info, "icon_zip")], "preview": "left: opaque (iOS), middle: transparent, right: Android adaptive foreground",
               "download_hint": 'curl -o icons.zip "<url>" && unzip icons.zip'}
    log_asset("export_icons", summary, [zip_info])
    return CallToolResult(content=[TextContent(type="text", text=json.dumps(summary, indent=2)), png_preview(preview, 800)])


# ---------------------------------------------------------------------------
# Project context + authorship record
# ---------------------------------------------------------------------------
@mcp.tool(structured_output=False)
async def set_project_context(
    project: str,
    art_direction: str = "",
    theme: str = "",
    lore: str = "",
    author: str = "",
    style_prompt: str = "",
    style_negative: str = "",
    default_preset: str = "",
    style_anchor: dict[str, str] | None = None,
) -> str:
    """Record the human creative direction for a game/project (plus an optional style lock) and make it active.

    Call this at the start of a work session (and whenever the direction changes). Every asset generated, edited,
    converted or packed afterwards is logged under this project together with its prompts, inputs, seeds and edits,
    building a dated authorship record of the owner's art direction, theme and lore behind each asset.
    Each change adds a new dated version that is a full snapshot (earlier versions are kept). Fields you leave
    empty carry over from the previous version; pass "-" to clear one. Calling with only `project` switches the
    server-wide active project to it (asset tools also take project=... per call, which is safer with several games).

    Args:
        project: short project name, e.g. "rune-quest".
        art_direction: the visual direction (style, palette, references, rules).
        theme: themes / tone / setting.
        lore: world, characters and story notes that the art expresses.
        author: who set this direction (person or studio).
        style_prompt: style lock: style words appended to every generate_image prompt for this project, e.g.
            "hand-painted 2D game art, thick dark outlines, warm muted palette, soft cel shading".
        style_negative: style lock: added to the negative prompt (presets that use one).
        default_preset: style lock: the preset generate_image uses when none is given.
        style_anchor: style lock: an approved image that defines the look, {"filename", "subfolder", "source"}
            (e.g. a generate_image result) or {"image_base64"}; {"clear": "true"} removes it. A copy is kept with
            the record. edit_image(match_style=true) uses it as a style reference.
    """
    name = _safe_name(project, "")
    if not name:
        raise ToolError("project name is required (letters, numbers, - or _).")
    if default_preset not in ("", "-") and default_preset not in PRESETS:
        raise ToolError(f"default_preset must be one of: {', '.join(PRESETS)}")
    ensure_record_dir()
    folder = PROVENANCE_DIR / "projects"
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / f"{name}.json"
    doc = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {"project": name, "versions": []}
    prev = doc["versions"][-1] if doc["versions"] else {}
    prev_style = prev.get("style") or {}

    def pick(new: str, old: str) -> str:
        return old if new == "" else ("" if new.strip() == "-" else new)

    fields = {k: pick(v, prev.get(k, "")) for k, v in
              (("author", author), ("art_direction", art_direction), ("theme", theme), ("lore", lore))}
    old_style = {"prompt": prev_style.get("prompt", ""), "negative": prev_style.get("negative", ""),
                 "preset": prev_style.get("preset", ""), "anchor": prev_style.get("anchor")}
    style = {"prompt": pick(style_prompt, old_style["prompt"]), "negative": pick(style_negative, old_style["negative"]),
             "preset": pick(default_preset, old_style["preset"]), "anchor": old_style["anchor"]}
    new_version = len(doc["versions"]) + 1
    if style_anchor:
        if str(style_anchor.get("clear", "")).lower() in ("1", "true", "yes"):
            style["anchor"] = None
        else:
            data, shown = await read_ref(style_anchor)
            try:
                im = PILImage.open(io.BytesIO(data))
                im.load()
            except Exception:  # noqa: BLE001
                raise ToolError("style_anchor is not a readable image.") from None
            buf = io.BytesIO()
            im.save(buf, "PNG")
            png = buf.getvalue()
            digest = hashlib.sha256(png).hexdigest()
            if (old_style["anchor"] or {}).get("sha256") != digest:
                stem = f"{name}_v{new_version}.png"
                (PROVENANCE_DIR / "anchors").mkdir(parents=True, exist_ok=True)
                (PROVENANCE_DIR / "anchors" / stem).write_bytes(png)
                style["anchor"] = {"file": f"anchors/{stem}", "comfy_input": f"style_anchors/{stem}", "sha256": digest,
                                   "source": shown}
                ensure_anchor_input(style["anchor"])
    if not prev and not any(fields.values()) and not any(style.values()):
        raise ToolError(f"Project {name!r} has no context yet; pass art_direction / theme / lore.")
    changed = not prev or fields != {k: prev.get(k, "") for k in fields} or style != old_style
    if changed:
        doc["versions"].append({"version": new_version, "time": _now_iso(), **fields, "style": style})
        path.write_text(json.dumps(doc, indent=2, ensure_ascii=False), encoding="utf-8")
    latest = doc["versions"][-1]
    (PROVENANCE_DIR / "active_project.json").write_text(json.dumps({"project": name, "version": latest["version"]}),
                                                        encoding="utf-8")
    cur = latest.get("style") or {}
    return json.dumps({
        "active_project": name, "context_version": latest["version"], "new_version_added": changed,
        "versions_on_record": len(doc["versions"]),
        "style_lock": {"style_prompt": cur.get("prompt", ""), "style_negative": cur.get("negative", ""),
                       "default_preset": cur.get("preset", ""), "style_anchor": (cur.get("anchor") or {}).get("file")},
        "note": "Pass project=" + json.dumps(name) + " on asset calls; they apply this style lock and are logged under "
                "it (see asset_history).",
    }, indent=2, ensure_ascii=False)


def ensure_anchor_input(anchor: dict[str, Any]) -> str:
    """Make sure the style anchor copy is in ComfyUI's input folder (restored from provenance if deleted)."""
    target = COMFY_DIRS["input"] / anchor["comfy_input"]
    if not target.is_file():
        src = PROVENANCE_DIR / anchor["file"]
        if not src.is_file():
            raise ToolError(f"The project's style anchor file is missing ({anchor['file']}); set a new style_anchor.")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, target)
    return f"{anchor['comfy_input']} [input]"


def _resolve_asset(ref: str) -> tuple[str, str]:
    """Find a server asset from a file name, 'subfolder/name' or 'output/subfolder/name'. Returns (path, sha256)."""
    ref = ref.strip().replace("\\", "/").strip("/")
    source = "output"
    for s in ("output/", "input/"):
        if ref.startswith(s):
            source, ref = s[:-1], ref[len(s):]
    base = COMFY_DIRS[source].resolve()
    if "/" in ref:
        candidates = [(base / ref).resolve()]
    else:
        candidates = [p.resolve() for p in base.rglob(ref) if p.is_file()]
        if len(candidates) > 1:
            shown = [f"{source}/{c.relative_to(base).as_posix()}" for c in candidates[:10]]
            raise ToolError(f"{ref!r} matches several files; pass subfolder/name, one of: {shown}")
    if not candidates or not candidates[0].is_file() or not candidates[0].is_relative_to(base):
        raise ToolError(f"Asset not found on the server: {source}/{ref}")
    p = candidates[0]
    return f"{source}/{p.relative_to(base).as_posix()}", hashlib.sha256(p.read_bytes()).hexdigest()


@mcp.tool(structured_output=False)
async def approve_asset(
    approved: list[str] | None = None,
    rejected: list[str] | None = None,
    notes: str = "",
    project: str = "",
) -> str:
    """Record the OWNER's choice among generated assets in the authorship record.

    Call this only after the owner has said which result(s) they pick or turn down (e.g. "use the second one,
    drop the rest"), and put their words in notes. Never call it on your own judgment: the record states that the
    owner made the selection. Selection, with the direction in set_project_context, is the human authorship this
    record documents. Each file is stored with its sha256, so the record stays tied to the exact image.

    Args:
        approved: assets the owner picked: file names as returned ("asset_00003_.png"), or "subfolder/name".
        rejected: assets the owner turned down (same forms).
        notes: the owner's words or reasons, e.g. "the brighter one fits the forest level".
        project: the game this is for (default: the active project).
    """
    if not approved and not rejected:
        raise ToolError("Pass approved and/or rejected file names.")
    use_project(project)
    picks = {"approved": [], "rejected": []}
    for key, items in (("approved", approved or []), ("rejected", rejected or [])):
        for ref in items:
            path, digest = _resolve_asset(ref)
            picks[key].append({"file": path, "sha256": digest})
    summary: dict[str, Any] = {**picks, "notes": notes}
    log_asset("approve_asset", summary, [])
    summary["note"] = "Recorded as the owner's selection (see asset_history)."
    return json.dumps(summary, indent=2, ensure_ascii=False)


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True), structured_output=False)
async def asset_history(filename: str = "", project: str = "", limit: int = 50) -> str:
    """Show the authorship record: how assets were made (prompts, inputs, edits, seeds, tools, dates) and the
    project direction/theme/lore versions they were made under.

    Args:
        filename: an asset's file name (or part of it) to see what created it and what it was later used in.
        project: show records for one project (default: the active project when no filename is given).
        limit: maximum records returned (newest last).
    """
    log_path = PROVENANCE_DIR / "assets.jsonl"
    records = []
    if log_path.exists():
        for line in log_path.read_text(encoding="utf-8").splitlines():
            try:
                records.append(json.loads(line))
            except ValueError:
                continue
    if not filename and not project:
        project = (_active_project() or {}).get("project", "")
    if project:
        records = [r for r in records if r.get("project") == _safe_name(project, "")]
    out: dict[str, Any] = {}

    def is_decision(r: dict[str, Any]) -> bool:
        return r.get("tool") == "approve_asset"

    if filename:
        created = [r for r in records if not is_decision(r) and any(filename in o for o in r.get("outputs", []))]
        decisions = [r for r in records if is_decision(r) and filename in json.dumps(r.get("details", {}))]
        used = [r for r in records if not is_decision(r) and r not in created
                and filename in json.dumps(r.get("details", {}))]
        out = {"created_by": created[-limit:], "owner_decisions": decisions[-limit:], "used_in": used[-limit:]}
        projects = {r.get("project") for r in created + decisions + used if r.get("project")}
    else:
        latest: dict[str, str] = {}
        for r in filter(is_decision, records):
            for key in ("approved", "rejected"):
                for item in r.get("details", {}).get(key, []):
                    latest[item["file"]] = key
        out = {"records": records[-limit:], "total_records": len(records),
               "owner_selection": {k: sorted(f for f, v in latest.items() if v == k) for k in ("approved", "rejected")}}
        projects = {project} if project else set()
    contexts = {}
    for p in projects:
        f = PROVENANCE_DIR / "projects" / f"{p}.json"
        if f.exists():
            contexts[p] = json.loads(f.read_text(encoding="utf-8"))["versions"]
    out["project_context"] = contexts
    return json.dumps(out, indent=2, ensure_ascii=False)


# ---------------------------------------------------------------------------
# Background jobs
# ---------------------------------------------------------------------------
JOB_TOOLS = {
    "generate_image": generate_image,
    "edit_image": edit_image,
    "edit_region": edit_region,
    "repaint_frames": repaint_frames,
    "animate_from_clip": animate_from_clip,
    "animate_loop": animate_loop,
    "image_to_3d": image_to_3d,
    "remove_background": remove_background,
    "run_workflow": run_workflow,
    "optimize_glb": optimize_glb,
    "pack_spritesheet": pack_spritesheet,
    "export_icons": export_icons,
}


def _job_directory() -> Path:
    identity = [COMFY_URL, MCP_PORT, *(os.path.normcase(os.path.realpath(COMFY_DIRS[kind]))
                                    for kind in ("input", "output"))]
    backend = hashlib.sha256(json.dumps(identity).encode()).hexdigest()[:24]
    return DATA_DIR / "jobs" / backend


def _remember_job(job: dict[str, Any]) -> bool:
    """Record only recovery metadata. URLs, prompts, previews and errors stay in memory."""
    from tempfile import NamedTemporaryFile

    directory = _job_directory()
    fields = ("id", "tool", "status", "created", "started", "finished", "prompt_ids",
              "submission_pending", "outputs", "cancel_targets", "cancel_acknowledged")
    metadata = {key: job[key] for key in fields if key in job}
    temporary = None
    try:
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        with NamedTemporaryFile(dir=directory, encoding="utf-8", mode="w", delete=False) as file:
            temporary = Path(file.name)
            json.dump(metadata, file, ensure_ascii=True, separators=(",", ":"))
            file.flush()
            os.fsync(file.fileno())
        temporary.replace(directory / f"{job['id']}.json")
        return True
    except OSError as error:
        log(f"could not save job metadata ({type(error).__name__})")
        return False
    finally:
        if temporary is not None:
            try:
                temporary.unlink(missing_ok=True)
            except OSError:
                pass


def _job_output_reference(item: Any) -> dict[str, str] | None:
    if not isinstance(item, dict) or item.get("type", "output") != "output":
        return None
    filename, subfolder = item.get("filename"), item.get("subfolder", "")
    if (not isinstance(filename, str) or not 0 < len(filename) <= 255
            or filename in {".", ".."} or any(c in filename for c in ("/", "\\", ":"))
            or not isinstance(subfolder, str) or len(subfolder) > 512
            or any(c in subfolder for c in ("\\", ":")) or subfolder.startswith("/")
            or ".." in subfolder.split("/") or any(ord(c) < 32 for c in filename + subfolder)):
        return None
    ref = {"filename": filename, "subfolder": subfolder, "type": "output"}
    for key in ("kind", "role"):
        value = item.get(key)
        if isinstance(value, str) and 0 < len(value) < 40 and all(c.isalnum() or c in "_-" for c in value):
            ref[key] = value
    return ref


def _remember_job_outputs(job: dict[str, Any], outputs: list[dict[str, Any]]) -> None:
    known = job.setdefault("outputs", [])
    before = len(known)
    seen = {(item["subfolder"], item["filename"]) for item in known}
    for item in outputs:
        if len(known) >= JOB_OUTPUT_LIMIT:
            break
        ref = _job_output_reference(item)
        if ref is None:
            continue
        key = ref["subfolder"], ref["filename"]
        if key not in seen:
            known.append(ref)
            seen.add(key)
    if len(known) != before:
        _remember_job(job)


def _restore_jobs() -> None:
    for path in _job_directory().glob("*.json"):
        try:
            if not re.fullmatch(r"[0-9a-f]{12}", path.stem) or path.stat().st_size > 2 * 1024 * 1024:
                continue
            job = json.loads(path.read_text(encoding="utf-8"))
            if (not isinstance(job, dict) or job.get("id") != path.stem or job.get("tool") not in JOB_TOOLS
                    or job.get("status") not in JOB_ACTIVE | {"done", "cancelled", "interrupted", "error"}
                    or not isinstance(job.get("created"), (int, float)) or not math.isfinite(job["created"])
                    or not isinstance(job.get("prompt_ids"), list)
                    or not all(isinstance(pid, str) and JOB_PROMPT_ID.fullmatch(pid) for pid in job["prompt_ids"])):
                continue
            if any(key in job and (not isinstance(job[key], (int, float)) or not math.isfinite(job[key]))
                   for key in ("started", "finished")):
                continue
            refs = job.get("outputs", [])
            targets = job.get("cancel_targets", {})
            acknowledged = job.get("cancel_acknowledged", [])
            if (not isinstance(refs, list) or not isinstance(targets, dict) or not isinstance(acknowledged, list)
                    or any(pid not in job["prompt_ids"] or state not in ("pending", "running")
                           for pid, state in targets.items())
                    or any(not isinstance(pid, str) or pid not in targets for pid in acknowledged)):
                continue
            job["outputs"] = [ref for item in refs[:JOB_OUTPUT_LIMIT] if (ref := _job_output_reference(item))]
            if job["status"] in {"queued", "running"}:
                job["status"] = "recovering"
            job.update(label="", recovered=True)
            if job["status"] == "error":
                job["error"] = "The job failed before Render Bridge restarted."
            _jobs[path.stem] = job
        except (OSError, ValueError, TypeError, RecursionError):
            continue
    _prune_jobs()


def _prune_jobs() -> None:
    now = time.time()
    inactive = []
    for job in _jobs.values():
        if any(task is not None and not task.done() for task in (job.get("task"), job.get("cancel_task"))):
            continue
        inactive.append(job)
    inactive.sort(key=lambda job: job.get("finished", job["created"]), reverse=True)
    for index, job in enumerate(inactive):
        if index >= JOB_HISTORY_LIMIT or now - job.get("finished", job["created"]) > JOB_RETENTION_S:
            try:
                (_job_directory() / f"{job['id']}.json").unlink(missing_ok=True)
            except OSError:
                continue
            del _jobs[job["id"]]


async def _upstream_queue() -> dict[str, str] | None:
    try:
        response = await _http.get("/queue", timeout=5)
        response.raise_for_status()
        queue = response.json()
        prompts = {}
        for key, state in (("queue_pending", "pending"), ("queue_running", "running")):
            rows = queue[key]
            if not isinstance(rows, list) or any(not isinstance(row, list) or len(row) < 2
                                                 or not isinstance(row[1], str) for row in rows):
                return None
            prompts.update((row[1], state) for row in rows)
        return prompts
    except (httpx.HTTPError, ValueError, TypeError, KeyError):
        return None


async def _request_prompt_cancel(prompt_id: str) -> str:
    """Ask ComfyUI to stop this exact prompt, never fall back to a global interrupt."""
    try:
        response = await _http.post(f"/api/jobs/{quote(prompt_id, safe='')}/cancel", timeout=5)
        if response.status_code in (404, 405):
            return "unsupported"
        if response.is_success and response.json().get("cancelled") is True:
            return "accepted"
    except (httpx.HTTPError, ValueError, AttributeError):
        pass
    return "unknown"


async def _job_history(prompt_id: str) -> dict[str, Any] | None:
    try:
        response = await _http.get(f"/history/{quote(prompt_id, safe='')}", timeout=5)
        response.raise_for_status()
        history = response.json()
        if isinstance(history, dict):
            entry = history.get(prompt_id, {})
            if isinstance(entry, dict):
                return entry
    except (httpx.HTTPError, ValueError):
        pass
    return None


def _history_outputs(entry: dict[str, Any]) -> list[dict[str, Any]]:
    outputs = entry.get("outputs", {})
    return [item for node in outputs.values() if isinstance(node, dict)
            for items in node.values() if isinstance(items, list)
            for item in items if isinstance(item, dict)] if isinstance(outputs, dict) else []


def _was_interrupted(entry: dict[str, Any]) -> bool:
    status = entry.get("status", {})
    messages = status.get("messages", []) if isinstance(status, dict) else []
    return isinstance(messages, list) and any(
        isinstance(message, list) and bool(message) and message[0] == "execution_interrupted"
        for message in messages)


def _finish_job(job: dict[str, Any], status: str, error: str | None = None) -> None:
    job.update(status=status, error=error, finished=time.time())
    _remember_job(job)


async def _recover_prompt_history(job: dict[str, Any]) -> dict[str, dict[str, Any] | None]:
    history = {}
    prompts = job["prompt_ids"]
    for index in range(0, len(prompts), 8):
        group = prompts[index:index + 8]
        entries = await asyncio.gather(*(_job_history(pid) for pid in group))
        history.update(zip(group, entries))
        _remember_job_outputs(job, [item for entry in entries if entry for item in _history_outputs(entry)])
    return history


async def _refresh_background_job(job: dict[str, Any]) -> None:
    status = job["status"]
    if status not in {"recovering", "cancel_requested", "cancel_unknown"}:
        return
    prompts = job["prompt_ids"]
    queue = await _upstream_queue() if prompts else {}
    if queue is None:
        if status != "recovering" and status != "cancel_unknown":
            job["status"] = "cancel_unknown"
            _remember_job(job)
        return
    active = {prompt for prompt in prompts if prompt in queue}
    job["gpu"] = ("running now" if any(queue[prompt] == "running" for prompt in active)
                  else "waiting in queue") if active else None
    if active:
        if status == "cancel_requested" and any(
                queue[pid] == "running" and job.get("cancel_targets", {}).get(pid) == "pending"
                for pid in active):
            job["status"] = "cancel_unknown"
            _remember_job(job)
        return
    history = await _recover_prompt_history(job)
    if status == "recovering":
        if any(entry is None for entry in history.values()):
            return
        note = ("Render Bridge restarted before the tool finished. ComfyUI may have accepted a submission "
                "whose prompt ID was never confirmed." if job.get("submission_pending")
                else "Render Bridge restarted before the tool finished. Available output files may be incomplete.")
        _finish_job(job, "interrupted", note)
        return
    targets, acknowledged = job.get("cancel_targets"), job.get("cancel_acknowledged", [])
    if targets is not None and not job.get("submission_pending"):
        if not prompts:
            _finish_job(job, "cancelled")
            return
        unconfirmed = [pid for pid, kind in targets.items()
                       if (entry := history[pid]) is None or not (_was_interrupted(entry) or
                           (not entry and pid in acknowledged and kind == "pending"))]
        if targets and not unconfirmed:
            _finish_job(job, "cancelled")
            return
        if all(history[pid] for pid in (unconfirmed or prompts)):
            if job.get("result") is not None:
                _finish_job(job, "done")
            else:
                _finish_job(job, "interrupted", "ComfyUI finished work before cancellation was confirmed. "
                            "Render Bridge stopped its local task. Available output files may be incomplete.")
            return
    if status != "cancel_unknown":
        job["status"] = "cancel_unknown"
        _remember_job(job)


async def _cancel_background_job(job: dict[str, Any]) -> None:
    prompts = job["prompt_ids"]
    queue = await _upstream_queue() if prompts else {}
    if queue is None:
        job["status"] = "cancel_unknown"
        _remember_job(job)
        return
    targets = {pid: queue[pid] for pid in prompts if pid in queue}
    job["cancel_targets"] = targets
    job["cancel_acknowledged"] = []
    _remember_job(job)
    acknowledged, legacy_pending = [], []
    for pid, state in targets.items():
        outcome = await _request_prompt_cancel(pid)
        if outcome == "accepted":
            acknowledged.append(pid)
        elif outcome == "unsupported" and state == "pending":
            legacy_pending.append(pid)
    if legacy_pending:
        # Older ComfyUI may accept this HTTP request without deleting a
        # prompt that started in the meantime. Do not treat it as proof.
        await _dequeue(*legacy_pending)
    job["cancel_acknowledged"] = acknowledged
    if len(acknowledged) != len(targets) or job.get("submission_pending"):
        job["status"] = "cancel_unknown"
    _remember_job(job)
    await _refresh_background_job(job)


def _job_public(job: dict[str, Any]) -> dict[str, Any]:
    end = job.get("finished") or time.time()
    result = {
        "job_id": job["id"], "tool": job["tool"], "label": job.get("label", ""), "status": job["status"],
        "created": time.strftime("%H:%M:%S", time.localtime(job["created"])),
        "elapsed_seconds": round(end - (job.get("started") or job["created"])),
        "error": job.get("error"),
    }
    if job.get("recovered"):
        result["recovered"] = True
    if job.get("gpu"):
        result["gpu"] = job["gpu"]
    if job["status"] == "cancel_unknown":
        result["note"] = "ComfyUI might still be running this work. Check its queue or try cancellation again."
    return result


def _recovered_outputs(job: dict[str, Any]) -> list[dict[str, Any]]:
    return [{**ref, "url": signed_url(ref),
             "urls": {label: signed_url(ref, base) for label, base in PUBLIC_URLS.items()}}
            for ref in job.get("outputs", [])]


async def _job_progress(job: dict[str, Any]) -> dict[str, Any]:
    try:
        q = (await _http.get("/queue", timeout=5)).json()
    except Exception:  # noqa: BLE001
        return {}
    running = [it[1] for it in q.get("queue_running", [])]
    pending = [it[1] for it in sorted(q.get("queue_pending", []), key=lambda it: it[0])]
    for pid in reversed(job["prompt_ids"]):
        if pid in running:
            return {"gpu": "running now"}
        if pid in pending:
            return {"gpu": f"waiting in queue (position {pending.index(pid) + 1} of {len(pending)})"}
    return {"gpu": "preparing" if not job["prompt_ids"] else "finishing"}


@mcp.tool(structured_output=False)
async def start_job(tool: str, arguments: dict[str, Any] | None = None, label: str = "") -> str:
    """Run generate_image, edit_image, edit_region, repaint_frames, animate_from_clip, animate_loop, image_to_3d,
    remove_background or run_workflow in the BACKGROUND.

    Returns a job_id immediately. Use it for anything slow (image_to_3d ~1-3 min, qwen_image / cinematic renders,
    or batches of several assets) so calls can't time out and you can queue many jobs at once; the GPU runs them
    one after another. Then call get_job(job_id, wait_seconds=60) until status is "done": it returns exactly what
    the tool itself would have returned (download URLs + previews).

    Args:
        tool: name of the tool to run.
        arguments: that tool's arguments, e.g. {"prompt": "...", "preset": "anime", "transparent": true}.
        label: optional note to recognise the job in list_jobs (e.g. "hero sprite v2").
    """
    if PAUSE_FLAG.exists():
        raise ToolError(PAUSED_MSG)
    fn = JOB_TOOLS.get(tool)
    if fn is None:
        raise ToolError(f"tool must be one of: {', '.join(JOB_TOOLS)}")
    arguments = dict(arguments or {})
    try:
        inspect.signature(fn).bind(**arguments)
    except TypeError as e:
        raise ToolError(f"Bad arguments for {tool}: {e}") from None
    now = time.time()
    _prune_jobs()
    job: dict[str, Any] = {"id": uuid.uuid4().hex[:12], "tool": tool, "label": label, "status": "queued", "created": now,
                           "prompt_ids": [], "result": None, "error": None}

    async def runner() -> None:
        _current_job.set(job)
        job.update(status="running", started=time.time())
        _remember_job(job)
        try:
            job["result"] = await fn(**arguments)
            if job["status"] == "running":
                _finish_job(job, "done")
        except asyncio.CancelledError:
            if job["status"] == "running":
                job.update(status="recovering", recovered=True)
                _remember_job(job)
            raise
        except Exception as e:  # noqa: BLE001
            if job["status"] == "running":
                _finish_job(job, "error", f"{type(e).__name__}: {e}"[:2000])
        finally:
            log(f"job {job['id']} ({tool}) {job['status']}")

    if not _remember_job(job):
        raise ToolError("Could not record the background job. Check the Render Bridge data directory.")
    _jobs[job["id"]] = job
    job["task"] = asyncio.create_task(runner())
    return json.dumps({"job_id": job["id"], "status": "queued", "next": f'get_job("{job["id"]}", wait_seconds=60)'})


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True), structured_output=False)
async def get_job(job_id: str, wait_seconds: int = 0) -> CallToolResult:
    """Check a background job from start_job. wait_seconds (0-120) waits for it to finish first.

    Returns the full live result when available. After restart, completed jobs retain downloadable file references
    and interrupted jobs may have partial outputs. Check again for "recovering" or "cancel_requested".
    """
    _prune_jobs()
    job = _jobs.get(job_id)
    if job is None:
        raise ToolError(f"No job {job_id!r} (up to {JOB_HISTORY_LIMIT} recent jobs are kept for up to 24 h).")
    task = job.get("task")
    if wait_seconds > 0 and task is not None and job["status"] in ("queued", "running"):
        try:
            await asyncio.wait_for(asyncio.shield(task), timeout=min(int(wait_seconds), 120))
        except asyncio.TimeoutError:
            pass
        except asyncio.CancelledError:
            if not task.cancelled():
                raise
    if job["status"] in JOB_ACTIVE - {"queued", "running"}:
        async with job.setdefault("refresh_lock", asyncio.Lock()):
            cancel_task = job.get("cancel_task")
            if cancel_task is None or cancel_task.done():
                await _refresh_background_job(job)
    pub = _job_public(job)
    res = job.get("result")
    if job["status"] == "done" and res is not None:
        return CallToolResult(content=[TextContent(type="text", text=json.dumps(pub)), *res.content], is_error=res.is_error)
    if job.get("outputs"):
        pub["outputs"] = _recovered_outputs(job)
    if job["status"] in {"queued", "running"}:
        pub.update(await _job_progress(job))
    return CallToolResult(content=[TextContent(type="text", text=json.dumps(pub, indent=2))], is_error=job["status"] == "error")


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True), structured_output=False)
async def list_jobs() -> str:
    """List background jobs (newest first) with their status."""
    _prune_jobs()
    jobs = sorted(_jobs.values(), key=lambda j: j["created"], reverse=True)
    return json.dumps([_job_public(j) for j in jobs], indent=2)


@mcp.tool(structured_output=False)
async def cancel_job(job_id: str) -> str:
    """Request cancellation. Poll get_job for confirmation or retry if the outcome is unknown."""
    _prune_jobs()
    job = _jobs.get(job_id)
    if job is None:
        raise ToolError(f"No job {job_id!r}.")
    if job["status"] not in JOB_ACTIVE:
        return json.dumps({"job_id": job_id, "status": job["status"], "note": "already finished"})
    task = job.get("cancel_task")
    if task is None or task.done():
        async with job.setdefault("refresh_lock", asyncio.Lock()):
            if job["status"] not in JOB_ACTIVE:
                return json.dumps({"job_id": job_id, "status": job["status"], "note": "already finished"})
            task = job.get("cancel_task")
            if task is None or task.done():
                job["status"] = "cancel_requested"
                _remember_job(job)
                runner = job.get("task")
                if runner is not None and not runner.done():
                    runner.cancel()
                task = job["cancel_task"] = asyncio.create_task(_cancel_background_job(job))
    await asyncio.shield(task)
    notes = {
        "cancel_requested": "ComfyUI accepted the request. Check get_job to confirm that it stopped.",
        "cancel_unknown": "Could not confirm cancellation. ComfyUI might still be running this work.",
        "cancelled": "Cancellation confirmed. Any files already written remain available.",
        "done": "The job finished before cancellation was confirmed.",
        "interrupted": "Render Bridge stopped its local task, and ComfyUI finished work before cancellation was confirmed.",
    }
    return json.dumps({"job_id": job_id, "status": job["status"], "note": notes[job["status"]]})


# ---------------------------------------------------------------------------
# Uploads: files sent to the server
# ---------------------------------------------------------------------------
_WIN_RESERVED = {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in range(10)), *(f"LPT{i}" for i in range(10))}
_uploads_used: dict[str, float] = {}  # signature -> expiry: each upload link works once


def _safe_filename(name: str) -> str:
    """A plain Windows file name: no folders, only ASCII letters, digits and - _ . , and no device names."""
    base = name.replace("\\", "/").rsplit("/", 1)[-1]
    clean = "".join(c if c.isascii() and (c.isalnum() or c in "-_.") else "_" for c in base).strip("._")
    stem, dot, ext = clean.rpartition(".")
    if not dot:
        stem, ext = clean, ""
    stem, ext = stem[:120] or "upload", ext[:16]
    if stem.split(".")[0].upper() in _WIN_RESERVED:
        stem = "_" + stem
    return f"{stem}.{ext}" if ext else stem


@mcp.tool(structured_output=False)
async def upload_link(filename: str, project: str = "", minutes: int = 60) -> str:
    """Get a one-time upload URL for a file, with the size limit in the response.

    Then send the file with:  curl -sS -T "<local file>" "<url>"
    The signed link works once without a bearer header. "urls" has one link per configured transfer address.
    The request returns JSON with the stored file's filename, subfolder and source="input" (pass
    them to edit_image, remove_background, image_to_3d, optimize_glb or set_project_context's style_anchor), its
    sha256, a ref for approve_asset and asset_history, and download links. An existing file is never overwritten:
    a taken name gets _2, _3, ... It only writes to disk, so it also works while gaming mode is on. Prefer it to
    image_base64 for anything but small images.

    Args:
        filename: the file's name (only the name is used, cleaned to letters, digits, - _ and .).
        project: the game it belongs to; the upload is logged under it in the authorship record (default: the
            active project).
        minutes: how long the link stays valid (1 to 1440).
    """
    name = _safe_filename(filename)
    proj = use_project(project)[0] or ""
    folder = f"{UPLOAD_SUBFOLDER}/{proj or 'misc'}"
    exp = int(time.time() + max(1, min(int(minutes), 1440)) * 60)
    nonce = secrets.token_hex(8)  # every link is distinct, even for the same name in the same second
    q = urlencode({"folder": folder, "project": proj, "exp": exp, "n": nonce,
                   "sig": _upload_sig(folder, name, proj, exp, nonce)})
    urls = {label: f"{base}/upload/{quote(name)}?{q}" for label, base in PUBLIC_URLS.items()}
    url = next(iter(urls.values()))
    return json.dumps({
        "url": url,
        "urls": urls,
        "curl": f'curl -sS -T "<local file>" "{url}"',
        "stores_as": f"input/{folder}/{name} (or {name.rpartition('.')[0] or name}_2... if taken)",
        "project": proj or None,
        "max_mb": UPLOAD_MAX_BYTES // 2**20,
        "expires": time.strftime("%Y-%m-%d %H:%M", time.localtime(exp)),
    }, indent=2)


class _UploadFailed(Exception):
    def __init__(self, status: int, text: str):
        super().__init__(text)
        self.status, self.text = status, text


def _place_upload(part: Path, folder: Path, name: str) -> str:
    """Give a finished upload its name, adding _2, _3, ... rather than overwriting an existing file."""
    stem, dot, ext = name.rpartition(".")
    if not dot:
        stem, ext = name, ""
    for n in range(1, 1000):
        cand = name if n == 1 else f"{stem}_{n}{dot}{ext}"
        try:
            if os.name == "nt":
                os.rename(part, folder / cand)
            else:
                os.link(part, folder / cand)
                part.unlink()
            return cand
        except FileExistsError:
            continue
    raise OSError(f"too many files named like {name}")


def _log_upload(project: str, rel: str, sha: str, size: int, requested: str) -> None:
    """Record the upload metadata and hash in the local provenance log."""
    try:
        doc = _project_doc(project) if project else None
        version = doc["versions"][-1]["version"] if doc and doc.get("versions") else None
        details: dict[str, Any] = {"bytes": size, "via": "upload_link"}
        if not rel.endswith("/" + requested):
            details["requested_name"] = requested
        rec = {"time": _now_iso(), "tool": "upload", "project": project or None, "project_context_version": version,
               "job": None, "details": details, "outputs": [rel], "sha256": {rel: sha}}
        ensure_record_dir()
        with open(PROVENANCE_DIR / "assets.jsonl", "a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
    except Exception as e:  # noqa: BLE001
        log(f"provenance log failed for upload {rel}: {e!r}")


# ---------------------------------------------------------------------------
# HTTP app: signed downloads + bearer-token auth
# ---------------------------------------------------------------------------
async def serve_signed_file(scope: dict[str, Any], receive: Any, send: Any) -> None:
    filename = unquote(scope["path"][len("/files/"):])
    q = parse_qs((scope.get("query_string") or b"").decode())
    subfolder, ftype, sig = q.get("subfolder", [""])[0], q.get("type", ["output"])[0], q.get("sig", [""])[0]
    try:
        exp = int(q.get("exp", ["0"])[0])
    except ValueError:
        exp = 0
    if exp < time.time() or not hmac.compare_digest(sig, _link_sig(filename, subfolder, ftype, exp)):
        await PlainTextResponse("Download link is invalid or expired.", status_code=403)(scope, receive, send)
        return
    method = scope["method"]
    request = _http.build_request(
        method, comfy_view_url({"filename": filename, "subfolder": subfolder, "type": ftype}),
        headers={"Accept-Encoding": "identity"}, timeout=120,
    )
    try:
        upstream = await _http.send(request, stream=True)
    except httpx.HTTPError:
        await PlainTextResponse("ComfyUI is unavailable.", status_code=503)(scope, receive, send)
        return
    try:
        if upstream.status_code != 200:
            await PlainTextResponse("File not found.", status_code=404)(scope, receive, send)
            return
        headers = {"Content-Disposition": f'attachment; filename="{quote(filename)}"'}
        for name in ("content-length", "content-encoding"):
            if name in upstream.headers:
                headers[name] = upstream.headers[name]
        media_type = upstream.headers.get("content-type", "application/octet-stream")
        if method == "HEAD":
            response = Response(media_type=media_type, headers=headers)
            if "content-length" not in headers:
                del response.headers["content-length"]
        else:
            response = StreamingResponse(
                upstream.aiter_raw(chunk_size=64 << 10), media_type=media_type, headers=headers,
            )
        await response(scope, receive, send)
    finally:
        await upstream.aclose()


async def receive_signed_upload(scope: dict[str, Any], receive: Any, send: Any) -> None:
    """PUT /upload/<name>?folder&project&exp&sig (from upload_link): store the body in ComfyUI's input folder."""
    async def reply(status: int, text: str) -> None:
        await PlainTextResponse(text + "\n", status_code=status)(scope, receive, send)

    name = unquote(scope["path"][len("/upload/"):])
    q = parse_qs((scope.get("query_string") or b"").decode())
    folder, project, nonce, sig = (q.get(k, [""])[0] for k in ("folder", "project", "n", "sig"))
    try:
        exp = int(q.get("exp", ["0"])[0])
    except ValueError:
        exp = 0
    if scope.get("method") != "PUT":
        return await reply(405, 'Send the file with a PUT, for example:  curl -sS -T "<local file>" "<this link>"')
    if exp < time.time() or not nonce or not hmac.compare_digest(sig, _upload_sig(folder, name, project, exp, nonce)):
        return await reply(403, "Upload link is invalid or expired; get a new one with upload_link.")
    if sig in _uploads_used:
        return await reply(409, "This upload link was already used; get a new one with upload_link.")
    input_root = COMFY_DIRS["input"].resolve()
    dest_dir = (input_root / folder).resolve()
    if (_safe_filename(name) != name or not folder.startswith(UPLOAD_SUBFOLDER + "/")
            or not dest_dir.is_relative_to(input_root / UPLOAD_SUBFOLDER)):
        return await reply(400, "Bad upload link.")
    try:
        length = int(dict(scope.get("headers") or [])[b"content-length"])
    except (KeyError, ValueError):
        return await reply(411, 'Send the file with curl -T "<local file>" so the request carries its length.')
    if length <= 0:
        return await reply(400, "The file is empty.")
    if length > UPLOAD_MAX_BYTES:
        return await reply(413, f"The file is {length / 2**20:.0f} MB; the limit is {UPLOAD_MAX_BYTES // 2**20} MB "
                                "(UPLOAD_MAX_MB on the server).")
    now = time.time()
    for s, e in list(_uploads_used.items()):
        if e < now:
            del _uploads_used[s]
    _uploads_used[sig] = exp  # claimed now so the link can't run twice at once; released again if this upload fails
    part = dest_dir / f".upload_{uuid.uuid4().hex}.part"
    digest, got = hashlib.sha256(), 0
    try:
        await asyncio.to_thread(dest_dir.mkdir, parents=True, exist_ok=True)
        fh = await asyncio.to_thread(open, part, "wb")
        try:
            buf = bytearray()
            while True:
                msg = await receive()
                if msg["type"] == "http.disconnect":
                    raise _UploadFailed(0, "")  # the sender went away; nobody to answer
                chunk = msg.get("body", b"")
                got += len(chunk)
                if got > length:
                    raise _UploadFailed(400, "More data arrived than the request said it would send.")
                digest.update(chunk)
                buf += chunk
                if len(buf) >= 4 << 20:  # write in 4 MB blocks off the event loop
                    data, buf = bytes(buf), bytearray()
                    await asyncio.to_thread(fh.write, data)
                if not msg.get("more_body"):
                    break
            if buf:
                await asyncio.to_thread(fh.write, bytes(buf))
        finally:
            await asyncio.to_thread(fh.close)
        if got != length:
            raise _UploadFailed(400, f"Only {got} of {length} bytes arrived; send it again.")
        final = await asyncio.to_thread(_place_upload, part, dest_dir, name)
    except (_UploadFailed, OSError) as e:
        _uploads_used.pop(sig, None)
        try:
            await asyncio.to_thread(part.unlink, missing_ok=True)
        except OSError:
            pass
        if isinstance(e, OSError):
            log(f"upload of {name} failed: {e!r}")
            return await reply(500, "Could not store the upload.")
        if e.status:
            return await reply(e.status, e.text)
        return
    sha = digest.hexdigest()
    rel = f"input/{folder}/{final}"
    _log_upload(project, rel, sha, got, name)
    log(f"upload: {rel} ({got} bytes)")
    file = {"filename": final, "subfolder": folder, "type": "input"}
    body: dict[str, Any] = {
        "filename": final, "subfolder": folder, "source": "input", "bytes": got, "sha256": sha, "ref": rel,
        "project": project or None,
        "use": 'Pass filename + subfolder with source="input" to edit_image, remove_background, image_to_3d or '
               'optimize_glb, or {"filename", "subfolder", "source": "input"} as set_project_context style_anchor. '
               "ref works in approve_asset and asset_history.",
        "url": signed_url(file),
        "urls": {label: signed_url(file, base) for label, base in PUBLIC_URLS.items()},
    }
    if final != name:
        body["note"] = f"{name} was already taken, so this is stored as {final}."
    await JSONResponse(body, status_code=201)(scope, receive, send)


class BearerAuth:
    def __init__(self, app: Any, token: str):
        self.app = app
        self.expected = f"Bearer {token}".encode()

    async def __call__(self, scope: dict[str, Any], receive: Any, send: Any) -> None:
        if scope["type"] == "http":
            if scope.get("path") == "/health":
                status = "paused" if PAUSE_FLAG.exists() else "ok" if await comfy_up() else "stopped"
                body = {"mcp": "ok", "comfyui": status}
                if managed_id := os.environ.get("RENDER_BRIDGE_MANAGED_ID"):
                    body["wizard_session"] = managed_id
                await JSONResponse(body)(scope, receive, send)
                return
            if scope.get("path", "").startswith("/files/") and scope.get("method") in ("GET", "HEAD"):
                await serve_signed_file(scope, receive, send)
                return
            if scope.get("path", "").startswith("/upload/"):
                await receive_signed_upload(scope, receive, send)
                return
            supplied = dict(scope.get("headers") or []).get(b"authorization", b"")
            if not hmac.compare_digest(supplied, self.expected):
                await PlainTextResponse("Unauthorized", status_code=401)(scope, receive, send)
                return
        await self.app(scope, receive, send)


def bind_listen_sockets(hosts: list[str], port: int) -> list[socket.socket]:
    """One listening socket per address, exclusive on Windows (no other process can bind that exact address and port)."""
    socks: list[socket.socket] = []
    for host in hosts:
        sock = socket.socket(socket.AF_INET6 if ":" in host else socket.AF_INET, socket.SOCK_STREAM)
        if hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        try:
            sock.bind((host, port))
        except OSError as exc:
            sock.close()
            for s in socks:
                s.close()
            log(f"Refusing to start: cannot listen on {host}:{port} ({exc}).")
            raise SystemExit(1)
        socks.append(sock)
    return socks


def rotate_token() -> None:
    from setup_render_bridge import atomic_text

    if not MCP_HOSTS or not 1 <= MCP_PORT <= 65535:
        raise SystemExit("Set MCP_HOST and MCP_PORT to the stopped server's listen addresses and port.")
    # Reserve the configured listener while replacing its credential, so a
    # concurrent server startup cannot read the previous token and then bind.
    sockets = bind_listen_sockets(MCP_HOSTS, MCP_PORT)
    try:
        metadata = TOKEN_FILE.lstat()
        if not stat.S_ISREG(metadata.st_mode) or (
            getattr(metadata, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT
        ):
            raise ValueError("The token is not a regular file.")
        if metadata.st_size > 4096 or not re.fullmatch(
            r"[A-Za-z0-9_-]{32,}", TOKEN_FILE.read_text(encoding="utf-8").strip()
        ):
            raise ValueError("The existing file does not contain a Render Bridge token.")
        atomic_text(TOKEN_FILE, secrets.token_urlsafe(32) + "\n")
    except (OSError, ValueError):
        raise SystemExit("Could not rotate the existing token file. Check its path and permissions.") from None
    finally:
        for sock in sockets:
            sock.close()
    print("Token rotated. Start Render Bridge and reconnect its MCP clients.")


def set_gaming_mode(enabled: bool) -> None:
    if enabled:
        PAUSE_FLAG.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        try:
            with PAUSE_FLAG.open("x", encoding="utf-8") as flag:
                flag.write(time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()) + "\n")
        except FileExistsError:
            pass
    else:
        PAUSE_FLAG.unlink(missing_ok=True)


async def enable_gaming_mode() -> None:
    set_gaming_mode(True)
    try:
        response = await _http.get("/queue", timeout=5)
        response.raise_for_status()
        queue = response.json()
        running = queue.get("queue_running") if isinstance(queue, dict) else None
        pending = queue.get("queue_pending") if isinstance(queue, dict) else None
        if not isinstance(running, list) or not isinstance(pending, list):
            print("Gaming mode is on. Queue status is unknown. No model unload was requested.")
        elif running or pending:
            print("Gaming mode is on. The ComfyUI queue is busy. No model unload was requested.")
        elif PAUSE_FLAG.exists():
            response = await _http.post("/free", json={"unload_models": True, "free_memory": True}, timeout=10)
            response.raise_for_status()
            print("Gaming mode is on. ComfyUI accepted the model unload request.")
        else:
            print("Gaming mode was turned off before the unload request.")
    except (httpx.HTTPError, ValueError):
        print("Gaming mode is on. No model unload was confirmed by ComfyUI.")
    finally:
        await _http.aclose()


async def amain() -> None:
    validate_server_settings()
    app = mcp.streamable_http_app(stateless_http=True, host=MCP_HOSTS[0], max_request_body_size=64 * 1024 * 1024)
    sockets = bind_listen_sockets(MCP_HOSTS, MCP_PORT)
    try:
        token = load_token()
    except BaseException:
        for sock in sockets:
            sock.close()
        raise
    _restore_jobs()
    log(f"Render Bridge listening on {', '.join(MCP_HOSTS)} at port {MCP_PORT}.")
    # Signed upload and download URLs include their own credentials in query strings.
    managed = bool(os.environ.get("RENDER_BRIDGE_MANAGED_ID"))
    server = uvicorn.Server(uvicorn.Config(
        BearerAuth(app, token), host=MCP_HOSTS[0], port=MCP_PORT, log_level="info", access_log=False,
        timeout_graceful_shutdown=1 if managed else None,
    ))
    if managed:
        # Closing the wizard's stdin pipe also reaches Python through the
        # Windows venv redirector and stops the actual server process.
        import sys
        import threading

        def stop_on_wizard_exit() -> None:
            sys.stdin.read()
            server.should_exit = True

        threading.Thread(target=stop_on_wizard_exit, daemon=True).start()
    watcher = asyncio.create_task(idle_watcher())
    try:
        await server.serve(sockets=sockets)
    finally:
        watcher.cancel()
        await asyncio.gather(watcher, return_exceptions=True)
        await _http.aclose()
        for sock in sockets:
            sock.close()


def main() -> None:
    parser = argparse.ArgumentParser(description="Render Bridge MCP server and local controls")
    commands = parser.add_subparsers(dest="command")
    game = commands.add_parser("gaming-mode", help="Pause or resume new GPU work through this MCP server")
    game.add_argument("state", choices=("on", "off", "status", "toggle"))
    commands.add_parser("rotate-token", help="Replace the private token after stopping this server.")
    args = parser.parse_args()
    if args.command is None:
        asyncio.run(amain())
    elif args.command == "rotate-token":
        rotate_token()
    elif args.state == "on" or (args.state == "toggle" and not PAUSE_FLAG.exists()):
        asyncio.run(enable_gaming_mode())
    elif args.state in ("off", "toggle"):
        set_gaming_mode(False)
        print("Gaming mode is off.")
    else:
        print("Gaming mode is on." if PAUSE_FLAG.exists() else "Gaming mode is off.")


if __name__ == "__main__":
    main()
