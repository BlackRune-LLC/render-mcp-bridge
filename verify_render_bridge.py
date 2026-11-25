"""Verify a local Render Bridge and compare selected private MCP config entries."""

import argparse
import hmac
import http.client
import io
import json
import os
from pathlib import Path
import re
import secrets
import sys
import time
import tomllib
from urllib.parse import parse_qs, urlsplit

from setup_render_bridge import asset_folders_proof


LOOPBACK_URL = re.compile(r"http://(127\.0\.0\.1|\[::1\]):([0-9]{1,5})/mcp")
NAME = re.compile(r"[A-Za-z0-9_-]{1,80}")
TOKEN = re.compile(r"[A-Za-z0-9_-]{32,}")
FIRST_RENDER_PREFIX = re.compile(r"rb-first-render-[0-9a-f]{24}")
PROTOCOL_VERSION = "2025-11-25"
MAX_RESPONSE = 128 * 1024
MAX_CONFIG = 16 * 1024 * 1024
MAX_LINK = 4096
MAX_TEST_PNG = 8 * 1024 * 1024
PREVIEW_SIZE = 96
PREVIEW_MODE = "RENDER_BRIDGE_FIRST_RENDER_PREVIEW"


class VerificationError(Exception):
    """A fixed status code that is safe to print."""


class ArgumentParser(argparse.ArgumentParser):
    def error(self, _message):
        raise VerificationError("invalid_arguments")


def read_object(response, request_id):
    content_type = response.headers.get_content_type()
    if content_type == "application/json":
        body = response.read(MAX_RESPONSE + 1)
        if len(body) > MAX_RESPONSE:
            raise VerificationError("unexpected_response")
        message = json.loads(body)
        return message if isinstance(message, dict) and message.get("id") == request_id else None
    if content_type != "text/event-stream":
        return None

    total = 0
    data = []
    while total < MAX_RESPONSE:
        line = response.readline(MAX_RESPONSE - total + 1)
        if not line:
            return None
        total += len(line)
        if total > MAX_RESPONSE:
            return None
        line = line.rstrip(b"\r\n")
        if line.startswith(b"data:"):
            data.append(line[5:].removeprefix(b" "))
        elif not line and data:
            message = json.loads(b"\n".join(data))
            if isinstance(message, dict) and message.get("id") == request_id:
                return message
            data.clear()
    return None


def post_mcp(connection, headers, method, *, request_id=None, params=None):
    request = {"jsonrpc": "2.0", "method": method}
    if request_id is not None:
        request["id"] = request_id
    if params is not None:
        request["params"] = params

    try:
        connection.request("POST", "/mcp", json.dumps(request), headers)
        response = connection.getresponse()
        if response.status in (401, 403):
            raise VerificationError("unauthorized")
        if request_id is None and response.status == 202:
            if len(response.read(MAX_RESPONSE + 1)) > MAX_RESPONSE:
                raise VerificationError("unexpected_response")
            return {}, None
        if response.status != 200 or request_id is None:
            raise VerificationError("unexpected_response")
        message = read_object(response, request_id)
        if len(response.read(MAX_RESPONSE + 1)) > MAX_RESPONSE:
            raise VerificationError("unexpected_response")
        if not message or message.get("jsonrpc") != "2.0":
            raise VerificationError("unexpected_response")
        result = message.get("result")
        if not isinstance(result, dict):
            code = "server_status_failed" if method == "tools/call" else "unexpected_response"
            raise VerificationError(code)
        return result, response.getheader("Mcp-Session-Id")
    except OSError:
        raise VerificationError("unreachable") from None
    except (http.client.HTTPException, ValueError, RecursionError):
        raise VerificationError("unexpected_response") from None


def prove_listener(connection, token, expected_backend, expected_folders):
    challenge = secrets.token_hex(16)
    try:
        connection.request("GET", "/health", headers={"X-Render-Bridge-Challenge": challenge})
        response = connection.getresponse()
        body = response.read(MAX_RESPONSE + 1)
        if response.status != 200 or len(body) > MAX_RESPONSE:
            raise VerificationError("owner_unverified")
        health = json.loads(body)
        if not isinstance(health, dict):
            raise VerificationError("owner_unverified")
        supplied = health.get("installation_proof")
        managed = health.get("panel_managed")
        backend = health.get("comfyui_url")
        if type(managed) is not bool or not isinstance(backend, str) or not backend.isascii():
            raise VerificationError("owner_unverified")
        message = f"rb-health-v2:{challenge}:{int(managed)}:{backend}".encode()
        expected = hmac.new(token.encode(), message, "sha256").hexdigest()
        if not isinstance(supplied, str) or not supplied.isascii() or not hmac.compare_digest(supplied, expected):
            raise VerificationError("owner_unverified")
        if expected_backend is not None and backend != expected_backend:
            raise VerificationError("backend_mismatch")
        if expected_folders is not None:
            supplied = health.get("asset_folders_proof")
            expected = asset_folders_proof(token.encode(), challenge.encode(), *expected_folders)
            if not isinstance(supplied, str) or not supplied.isascii() or not hmac.compare_digest(supplied, expected):
                raise VerificationError("backend_mismatch")
        if connection.sock is None:
            raise VerificationError("owner_unverified")
        # Authorize only the socket that answered the installation challenge.
        connection.auto_open = False
        return managed
    except (OSError, http.client.HTTPException, ValueError, RecursionError):
        raise VerificationError("owner_unverified") from None


def verify_bridge(connection, token):
    headers = {
        "Accept": "application/json, text/event-stream",
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}",
    }
    result, session_id = post_mcp(connection, headers, "initialize", request_id=1, params={
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": {"name": "render-bridge-verify", "version": "1"},
    })
    version = result.get("protocolVersion")
    if not isinstance(version, str) or not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", version):
        raise VerificationError("unexpected_response")
    if not isinstance(result.get("serverInfo"), dict):
        raise VerificationError("unexpected_response")
    headers["MCP-Protocol-Version"] = version
    if session_id is not None:
        if not re.fullmatch(r"[\x21-\x7e]{1,256}", session_id):
            raise VerificationError("unexpected_response")
        headers["Mcp-Session-Id"] = session_id
    post_mcp(connection, headers, "notifications/initialized")
    result, _ = post_mcp(connection, headers, "tools/call", request_id=2, params={
        "name": "server_status",
        "arguments": {},
    })
    if result.get("isError"):
        return "server_status_failed"
    try:
        status = result.get("structuredContent")
        if status is None:
            content = result["content"]
            first = content[0]
            status = json.loads(first["text"]) if first["type"] == "text" else None
    except (IndexError, KeyError, TypeError, ValueError, RecursionError):
        return "server_status_failed"
    running = status.get("comfyui_running") if isinstance(status, dict) else None
    if type(running) is not bool:
        return "server_status_failed"
    return "ok" if running else "comfyui_unavailable"


def first_render_path(origin, prefix, link, token):
    if not link.isascii() or any(ord(char) < 33 or ord(char) == 127 for char in link):
        return None
    try:
        parsed = urlsplit(link)
        fields = parse_qs(parsed.query, strict_parsing=True, max_num_fields=4)
    except ValueError:
        return None
    if parsed.scheme != "http" or parsed.netloc != urlsplit(origin).netloc or parsed.fragment:
        return None
    filename = parsed.path.removeprefix("/files/")
    if not parsed.path.startswith("/files/") or not re.fullmatch(rf"{re.escape(prefix)}_[0-9]+_\.png", filename):
        return None
    if set(fields) != {"subfolder", "type", "exp", "sig"} or any(len(values) != 1 for values in fields.values()):
        return None
    subfolder, kind, expires, signature = (fields[key][0] for key in ("subfolder", "type", "exp", "sig"))
    if subfolder != "mcp" or kind != "output" or not re.fullmatch(r"[0-9]{1,12}", expires):
        return None
    if int(expires) < time.time() or not re.fullmatch(r"[0-9a-f]{40}", signature):
        return None
    message = f"output|mcp|{filename}|{int(expires)}".encode()
    expected = hmac.digest(token.encode(), message, "sha256").hex()[:40]
    if not hmac.compare_digest(expected, signature):
        return None
    return f"{parsed.path}?{parsed.query}"


def verify_first_render(endpoint, origin, prefix, link, token):
    path = first_render_path(origin, prefix, link, token)
    if path is None:
        return "wrong_link", None
    connection = http.client.HTTPConnection(endpoint[1].strip("[]"), int(endpoint[2]), timeout=8)
    try:
        connection.request("GET", path, headers={"Accept-Encoding": "identity"})
        with connection.getresponse() as response:
            if response.status != 200:
                return "download_failed", None
            data = response.read(MAX_TEST_PNG + 1)
    except (OSError, http.client.HTTPException):
        return "download_failed", None
    finally:
        connection.close()
    if len(data) > MAX_TEST_PNG:
        return "invalid_png", None
    from PIL import Image

    preview = None
    try:
        with Image.open(io.BytesIO(data), formats=("PNG",)) as image:
            if image.size != (512, 512):
                return "invalid_png", None
            image.verify()
        with Image.open(io.BytesIO(data), formats=("PNG",)) as image:
            image.load()
            if os.environ.get(PREVIEW_MODE) == "1":
                thumbnail = image.convert("RGBA").resize((PREVIEW_SIZE, PREVIEW_SIZE), Image.Resampling.LANCZOS)
                preview = thumbnail.tobytes().hex()
    except (OSError, ValueError, SyntaxError, Image.DecompressionBombError):
        return "invalid_png", None
    return "ok", preview


def load_config(path, server_table, parse):
    try:
        with path.open("rb") as file:
            data = file.read(MAX_CONFIG + 1)
    except FileNotFoundError:
        return None
    except OSError:
        raise VerificationError("config_unreadable") from None
    if len(data) > MAX_CONFIG:
        raise VerificationError("config_unreadable")
    try:
        config = parse(data.decode("utf-8-sig"))
    except (ValueError, RecursionError):
        raise VerificationError("config_unreadable") from None
    if not isinstance(config, dict) or not isinstance(config.get(server_table, {}), dict):
        raise VerificationError("config_unreadable")
    return config.get(server_table, {})


def authorization(headers):
    if not isinstance(headers, dict):
        raise VerificationError("config_unreadable")
    values = [value for key, value in headers.items() if key.lower() == "authorization"]
    if len(values) > 1:
        raise VerificationError("auth_conflict")
    if values and not isinstance(values[0], str):
        raise VerificationError("config_unreadable")
    return values[0] if values else None


def compare_entry(entry, client, url, token):
    if not isinstance(entry, dict):
        return "config_unreadable"
    if entry.get("enabled") is False:
        return "server_disabled"
    if client == "claude" and entry.get("type") != "http":
        return "config_conflict"
    if client == "codex" and ("command" in entry or entry.get("environment_id", "local") != "local"):
        return "config_conflict"
    if entry.get("url") != url:
        return "url_mismatch"

    header_key = "headers" if client == "claude" else "http_headers"
    supplied = authorization(entry.get(header_key, {}))
    if client == "codex":
        candidates = [supplied, entry.get("bearer_token_env_var"),
                      authorization(entry.get("env_http_headers", {})), entry.get("http_headers_helper")]
        sources = sum(candidate is not None for candidate in candidates)
        if sources > 1:
            return "auth_conflict"
        if supplied is None and sources:
            return "auth_unverified"
    if supplied is None:
        return "token_missing"
    if client == "claude" and "${" in supplied:
        return "auth_unverified"
    matches = hmac.compare_digest(supplied.encode("utf-8"), f"Bearer {token}".encode("ascii"))
    return "config_matches" if matches else "token_mismatch"


def verify_client(client, args, token):
    conflicts = set()
    if client == "claude":
        root = args.claude_config_dir or os.environ.get("CLAUDE_CONFIG_DIR")
        config = Path(root).expanduser() / ".claude.json" if root else Path.home() / ".claude.json"
        servers = load_config(config, "mcpServers", json.loads)
    else:
        root = args.codex_home or os.environ.get("CODEX_HOME")
        home = Path(root).expanduser() if root else Path.home() / ".codex"
        servers = load_config(home / "config.toml", "mcp_servers", tomllib.loads)
        if args.codex_profile:
            profile = load_config(home / f"{args.codex_profile}.config.toml", "mcp_servers", tomllib.loads)
            if profile is None:
                return "profile_missing"
            if servers is None:
                servers = {}
            conflicts = {name for name in servers.keys() & profile.keys() if servers[name] != profile[name]}
            servers.update(profile)
    if servers is None:
        return "config_missing"
    name = args.server_name or "comfyui"
    if args.server_name is None:
        preferred = servers.get(name)
        if not isinstance(preferred, dict) or preferred.get("url") != args.url:
            matches = [key for key, entry in servers.items()
                       if isinstance(entry, dict) and entry.get("url") == args.url]
            if len(matches) > 1:
                return "config_conflict"
            if matches:
                name = matches[0]
    if name not in servers:
        return "server_missing"
    if name in conflicts:
        return "config_conflict"
    return compare_entry(servers[name], client, args.url, token)


def arguments(argv):
    parser = ArgumentParser(description=__doc__)
    parser.add_argument("url", help="Local Render Bridge /mcp URL with an explicit port")
    parser.add_argument("token_file", type=Path, help="Path to the private Bridge token file")
    parser.add_argument("clients", nargs="*", metavar="claude|codex", help="User configs to check")
    parser.add_argument("--server-name", help="Check only this name instead of looking up the Bridge URL")
    parser.add_argument("--claude-config-dir", help="Selected CLAUDE_CONFIG_DIR")
    parser.add_argument("--codex-home", help="Selected CODEX_HOME")
    parser.add_argument("--codex-profile", help="Named Codex --profile layer, if used")
    parser.add_argument("--first-render", help="Check the test PNG link passed through standard input")
    parser.add_argument("--require-owner", action="store_true", help="Prove the listener owns this installation's token")
    parser.add_argument("--expected-backend", help="Check the proven listener uses this backend URL")
    parser.add_argument("--expected-folders", nargs=2, metavar=("INPUT", "OUTPUT"),
                        help="Check the proven listener uses these asset folders")
    args = parser.parse_intermixed_args(argv)
    if (any(client not in {"claude", "codex"} for client in args.clients)
            or (args.server_name is not None and not NAME.fullmatch(args.server_name))
            or (args.codex_profile is not None and not NAME.fullmatch(args.codex_profile))
            or (args.first_render is not None and (args.clients or not FIRST_RENDER_PREFIX.fullmatch(args.first_render)))):
        raise VerificationError("invalid_arguments")
    return args


def main(argv=None):
    try:
        args = arguments(argv)
    except VerificationError as error:
        print(json.dumps({"bridge": str(error)}))
        return 2

    endpoint = LOOPBACK_URL.fullmatch(args.url)
    if not endpoint or not 1 <= int(endpoint[2]) <= 65535:
        print(json.dumps({"bridge": "invalid_url"}))
        return 0

    try:
        with args.token_file.open("r", encoding="utf-8") as file:
            contents = file.read(4097)
        token = contents.strip()
        if len(contents) > 4096 or not TOKEN.fullmatch(token):
            raise ValueError
    except (OSError, ValueError):
        print(json.dumps({"first_render" if args.first_render else "bridge": "token_unavailable"}))
        return 0

    if args.first_render:
        preview = None
        data = sys.stdin.buffer.read(MAX_LINK + 1)
        if len(data) > MAX_LINK:
            status = "wrong_link"
        else:
            try:
                link = data.decode("ascii").strip()
            except UnicodeError:
                status = "wrong_link"
            else:
                status, preview = verify_first_render(endpoint, args.url.removesuffix("/mcp"), args.first_render, link, token)
        receipt = {"first_render": status}
        if preview is not None:
            receipt["preview_rgba"] = preview
        print(json.dumps(receipt))
        return 0

    # Use the literal loopback address without redirects or proxy settings.
    connection = http.client.HTTPConnection(endpoint[1].strip("[]"), int(endpoint[2]), timeout=12)
    result = {}
    try:
        if args.require_owner:
            result["panel_managed"] = prove_listener(connection, token, args.expected_backend, args.expected_folders)
        result["bridge"] = verify_bridge(connection, token)
    except VerificationError as error:
        result["bridge"] = str(error)
    finally:
        connection.close()
    for client in dict.fromkeys(args.clients):
        try:
            result[client] = verify_client(client, args, token)
        except VerificationError as error:
            result[client] = str(error)
        except (OSError, RuntimeError, ValueError):
            result[client] = "config_unreadable"
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
