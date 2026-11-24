"""Verify a local Render Bridge and compare selected private MCP config entries."""

import argparse
import hmac
import http.client
import json
import os
from pathlib import Path
import re
import tomllib


LOOPBACK_URL = re.compile(r"http://(127\.0\.0\.1|\[::1\]):([0-9]{1,5})/mcp")
NAME = re.compile(r"[A-Za-z0-9_-]{1,80}")
TOKEN = re.compile(r"[A-Za-z0-9_-]{32,}")
PROTOCOL_VERSION = "2025-11-25"
MAX_RESPONSE = 128 * 1024
MAX_CONFIG = 16 * 1024 * 1024


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


def post_mcp(host, port, headers, method, *, request_id=None, params=None):
    request = {"jsonrpc": "2.0", "method": method}
    if request_id is not None:
        request["id"] = request_id
    if params is not None:
        request["params"] = params

    # HTTPConnection connects to the literal loopback IP and neither reads proxy
    # settings nor follows redirects. Do not substitute a URL from client config.
    connection = http.client.HTTPConnection(host, port, timeout=12)
    try:
        connection.request("POST", "/mcp", json.dumps(request), headers)
        response = connection.getresponse()
        if response.status in (401, 403):
            raise VerificationError("unauthorized")
        if request_id is None and response.status == 202:
            return {}, None
        if response.status != 200 or request_id is None:
            raise VerificationError("unexpected_response")
        message = read_object(response, request_id)
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
    finally:
        connection.close()


def verify_bridge(host, port, token):
    headers = {
        "Accept": "application/json, text/event-stream",
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}",
    }
    result, session_id = post_mcp(host, port, headers, "initialize", request_id=1, params={
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
    post_mcp(host, port, headers, "notifications/initialized")
    result, _ = post_mcp(host, port, headers, "tools/call", request_id=2, params={
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
    args = parser.parse_intermixed_args(argv)
    if (any(client not in {"claude", "codex"} for client in args.clients)
            or (args.server_name is not None and not NAME.fullmatch(args.server_name))
            or (args.codex_profile is not None and not NAME.fullmatch(args.codex_profile))):
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
        print(json.dumps({"bridge": "token_unavailable"}))
        return 0

    try:
        bridge = verify_bridge(endpoint[1].strip("[]"), int(endpoint[2]), token)
    except VerificationError as error:
        bridge = str(error)
    result = {"bridge": bridge}
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
