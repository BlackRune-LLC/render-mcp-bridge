"""Set up and run Render Bridge with a locally managed ComfyUI backend."""

import argparse
import http.client
import ipaddress
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

PROJECT_ROOT = Path(__file__).resolve().parent
DEFAULT_URL = "http://127.0.0.1:8188"
MODEL_GUIDE = "https://github.com/BlackRune-LLC/render-mcp-bridge/blob/main/docs/pipelines.md"
CLIENT_GUIDE = "https://github.com/BlackRune-LLC/render-mcp-bridge#connect-an-mcp-client"
COMFY_INSTALL = "https://docs.comfy.org/installation/manual_install"
SETTINGS_KEYS = {"COMFY_ROOT", "COMFY_URL"}
OPTIONAL_SETTINGS = {"COMFY_INPUT_DIR", "COMFY_OUTPUT_DIR"}
MINIMUM_PYTHON = (3, 11)
PROBE_TIMEOUT = 3
PROBE_LIMIT = 1024 * 1024
STARTER_MODELS = (
    ("UNETLoader", "unet_name", "SwarmUI_Z-Image-Turbo-FP8Mix.safetensors"),
    ("CLIPLoader", "clip_name", "qwen_3_4b.safetensors"),
    ("VAELoader", "vae_name", "ae.safetensors"),
    ("CheckpointLoaderSimple", "ckpt_name", "NetaYumev35_pretrained_all_in_one.safetensors"),
)


def data_dir() -> Path:
    override = os.environ.get("COMFY_MCP_DATA_DIR")
    if override:
        return Path(override).expanduser().resolve()
    if os.name == "nt":
        base = os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local"
    else:
        base = os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share"
    return (Path(base).expanduser() / "comfy-mcp").resolve()


def load_settings(path: Path) -> dict[str, str]:
    try:
        settings = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}
    if (
        not isinstance(settings, dict)
        or not SETTINGS_KEYS <= settings.keys() <= SETTINGS_KEYS | OPTIONAL_SETTINGS
        or any(not isinstance(value, str) or not value for value in settings.values())
    ):
        raise ValueError("Invalid setup settings.")
    return settings


def save_settings(path: Path, settings: dict[str, str]) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".render-bridge-", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
            json.dump(settings, stream, indent=2, sort_keys=True)
            stream.write("\n")
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def backend_url(value: str) -> str:
    """Accept only unauthenticated HTTP(S) addresses on this machine."""
    value = value.strip()
    if not value or any(ord(character) < 33 or ord(character) > 126 for character in value):
        raise ValueError("Enter a local HTTP(S) address, such as http://127.0.0.1:8188.")
    try:
        address = urlsplit(value)
        host = address.hostname or ""
        local = host.lower() == "localhost" or ipaddress.ip_address(host).is_loopback
        authority = f"[{host}]" if ":" in host else host
        if address.port is not None:
            authority += f":{address.port}"
        valid = (
            local and address.scheme in {"http", "https"} and address.path in {"", "/"}
            and address.netloc.lower() == authority.lower()
            and "?" not in value and "#" not in value and "\\" not in value
            and address.port != 0
        )
    except ValueError:
        valid = False
    if not valid:
        raise ValueError("Use a loopback HTTP(S) address, with no credentials, path, query, or fragment.")
    return f"{address.scheme}://{authority}"


def comfy_root(value: str) -> Path:
    path = Path(value.strip().strip('"').strip("'")).expanduser().resolve()
    if not all((path / folder).is_dir() for folder in ("input", "output")):
        raise ValueError("Choose the ComfyUI folder that contains both input and output directories.")
    return path


def fetch_json(url: str, endpoint: str):
    address = urlsplit(url)
    connection_type = http.client.HTTPSConnection if address.scheme == "https" else http.client.HTTPConnection
    connection = connection_type(address.hostname, address.port, timeout=PROBE_TIMEOUT)
    try:
        connection.request("GET", endpoint, headers={"Accept": "application/json"})
        response = connection.getresponse()
        if response.status != 200:
            return None
        data = response.read(PROBE_LIMIT + 1)
        return json.loads(data) if len(data) <= PROBE_LIMIT else None
    except (OSError, http.client.HTTPException, ValueError, RecursionError):
        return None
    finally:
        connection.close()


def model_available(url: str, node: str, field: str, filename: str) -> bool | None:
    info = fetch_json(url, f"/object_info/{node}")
    try:
        spec = info[node]["input"]["required"][field]
        choices = spec[0] if isinstance(spec[0], list) else spec[1]["options"]
    except (IndexError, KeyError, TypeError):
        return None
    if not isinstance(choices, list) or any(not isinstance(choice, str) for choice in choices):
        return None
    return any(choice.replace("\\", "/").rsplit("/", 1)[-1] == filename for choice in choices)


def probe_backend(url: str) -> dict[str, bool | None] | None:
    stats = fetch_json(url, "/system_stats")
    if not isinstance(stats, dict) or not isinstance(stats.get("system"), dict):
        return None
    return {filename: model_available(url, node, field, filename) for node, field, filename in STARTER_MODELS}


def prompt_for(label, default, validate):
    while True:
        try:
            return validate(input(f"{label} [{default}]: ").strip() or default)
        except (ValueError, OSError) as error:
            print(str(error) if isinstance(error, ValueError) else "That directory could not be read.")


def venv_python() -> Path:
    executable = "Scripts/python.exe" if os.name == "nt" else "bin/python"
    return PROJECT_ROOT / ".venv" / executable


def quiet_run(command: list[str]) -> int:
    # Package-manager diagnostics can include private index or proxy settings.
    result = subprocess.run(
        command, check=False, cwd=PROJECT_ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    return result.returncode


def install(settings_path: Path) -> int:
    try:
        previous = load_settings(settings_path)
    except (OSError, ValueError):
        print("Previous installer settings could not be read. Enter your ComfyUI settings again.")
        previous = {}
    default_root = os.environ.get("COMFY_ROOT") or previous.get("COMFY_ROOT") or str(Path.home() / "ComfyUI")
    default_url = os.environ.get("COMFY_URL") or previous.get("COMFY_URL") or DEFAULT_URL
    try:
        default_url = backend_url(default_url)
    except ValueError:
        print("The configured ComfyUI address is not a valid loopback URL. Enter a new address.")
        default_url = DEFAULT_URL
    print("Render Bridge setup")
    print(f"Install ComfyUI first if you need it. {COMFY_INSTALL}")
    root = prompt_for("ComfyUI directory", default_root, comfy_root)
    url = prompt_for("ComfyUI API URL", default_url, backend_url)
    while True:
        print("Checking the local ComfyUI API and its model loaders.")
        models = probe_backend(url)
        if models is not None:
            print("ComfyUI responded. Model files reported by its loaders:")
            for filename, available in models.items():
                status = {True: "Found", False: "Missing", None: "Unknown"}[available]
                print(f"  {status}: {filename}")
            break
        print("ComfyUI did not respond with system information. Start it using the address above and retry.")
        if input("Retry the ComfyUI check? [y/N] ").strip().lower() not in {"y", "yes"}:
            print("Continuing setup. Start ComfyUI before using Render Bridge tools.")
            break
    print(f"For model installation and preset requirements, see docs/pipelines.md in this checkout. {MODEL_GUIDE}")
    if input("Create or reuse .venv and install dependencies? [y/N] ").strip().lower() not in {"y", "yes"}:
        print("Setup cancelled.")
        return 0

    python = venv_python()
    requirements = PROJECT_ROOT / "requirements.txt"
    if not requirements.is_file():
        print("requirements.txt is missing. Run setup from a complete Render Bridge checkout.", file=sys.stderr)
        return 1
    if not python.is_file():
        print("Creating the Render Bridge virtual environment.")
        if quiet_run([sys.executable, "-m", "venv", str(PROJECT_ROOT / ".venv")]) != 0:
            print("Could not create .venv. Check your Python installation and repository permissions.", file=sys.stderr)
            return 1
    if quiet_run([str(python), "-c", f"import sys; sys.exit(sys.version_info < {MINIMUM_PYTHON!r})"]) != 0:
        print("The virtual environment needs Python 3.11 or newer. Move the old .venv aside and rerun setup.", file=sys.stderr)
        return 1
    print("Installing the Python requirements. This may take a minute.")
    if quiet_run([str(python), "-m", "pip", "--disable-pip-version-check", "--no-input", "install", "-r", str(requirements)]) != 0:
        print("Dependency installation failed. Check your Python package index and connection, then rerun setup.", file=sys.stderr)
        return 1
    save_settings(settings_path, {"COMFY_ROOT": str(root), "COMFY_URL": url})
    token = Path(os.environ.get("MCP_TOKEN_FILE") or settings_path.parent / "token.txt").expanduser()
    if not token.is_absolute():
        token = PROJECT_ROOT / token
    print("Setup saved. Use the same Python launcher with setup_render_bridge.py start.")
    print(f"On first start, the server creates its private token file at {token}.")
    print(f"Register the server in your private MCP client settings. {CLIENT_GUIDE}")
    return 0


def launch(settings_path: Path, server_args: list[str]) -> int:
    python = venv_python()
    if not python.is_file():
        print("Run setup_render_bridge.py install to create the Render Bridge virtual environment.", file=sys.stderr)
        return 1
    try:
        settings = load_settings(settings_path)
        if not settings:
            raise ValueError("Missing setup settings.")
    except (OSError, ValueError):
        print("Run setup_render_bridge.py install to save the ComfyUI connection settings.", file=sys.stderr)
        return 1
    try:
        settings["COMFY_URL"] = backend_url(settings["COMFY_URL"])
        root = settings["COMFY_ROOT"]
        settings["COMFY_ROOT"] = str(Path(root).expanduser().resolve())
        for kind in ("input", "output"):
            key = f"COMFY_{kind.upper()}_DIR"
            folder = Path(settings[key]).expanduser().resolve() if key in settings else Path(settings["COMFY_ROOT"]) / kind
            if not server_args and not folder.is_dir():
                raise ValueError("Missing ComfyUI directory.")
            settings[key] = str(folder)
    except (OSError, ValueError):
        print("The saved ComfyUI directory or URL is invalid. Rerun setup.", file=sys.stderr)
        return 1
    env = {**os.environ, **settings, "COMFY_MCP_DATA_DIR": str(settings_path.parent)}
    server = PROJECT_ROOT / "comfy_mcp_server.py"
    return subprocess.run([str(python), str(server), *server_args], check=False, cwd=PROJECT_ROOT, env=env).returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Install, start, or control a local Render Bridge server.")
    parser.add_argument("--data-dir", type=Path, help="Use the selected Render Bridge installation data directory.")
    commands = parser.add_subparsers(dest="command")
    commands.add_parser("install", help="Run guided setup (the default).")
    commands.add_parser("start", help="Run the configured server in the foreground.")
    gaming = commands.add_parser("gaming-mode", help="Pause or resume work through the configured server.")
    gaming.add_argument("state", choices=("on", "off", "status", "toggle"))
    gaming.add_argument("--wait", action="store_true", help="Keep the shortcut's terminal open until Enter.")
    args = parser.parse_args(argv)
    if sys.version_info < MINIMUM_PYTHON:
        print("Render Bridge setup requires Python 3.11 or newer.", file=sys.stderr)
        return 1
    try:
        settings_path = (args.data_dir.expanduser().resolve() if args.data_dir else data_dir()) / "settings.json"
        if args.command in {None, "install"}:
            return install(settings_path)
        server_args = ["gaming-mode", args.state] if args.command == "gaming-mode" else []
        result = launch(settings_path, server_args)
        if args.command == "gaming-mode" and args.wait and sys.stdin.isatty():
            try:
                input("Press Enter to close this Gaming Mode window.")
            except EOFError:
                pass
        return result
    except (EOFError, KeyboardInterrupt):
        print("\nRender Bridge command stopped.", file=sys.stderr)
        return 130
    except (OSError, RuntimeError):
        print("A local file or process could not be accessed. Check the setup paths and permissions.", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
