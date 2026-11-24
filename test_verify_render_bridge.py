import hashlib
import hmac
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlencode
import zlib

from setup_render_bridge import asset_folders_proof


SCRIPT = Path(__file__).with_name("verify_render_bridge.py")
TOKEN = "a_test_token_that_will_never_be_used_for_auth"
RENDER_PREFIX = "rb-first-render-0123456789abcdef01234567"


def fixture_png(width=512, height=512):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    header = struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(bytes((width + 1) * height))) + chunk(b"IEND", b""))


class BridgeHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def reply(self, code, body=b"", **headers):
        self.send_response(code)
        self.send_header("Content-Length", str(len(body)))
        for key, value in headers.items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self.server.calls.append("owner")
            assert self.headers.get("Authorization") is None
            challenge = self.headers["X-Render-Bridge-Challenge"]
            managed = self.server.panel_managed
            backend = "http://127.0.0.1:8188"
            message = f"rb-health-v2:{challenge}:{int(managed)}:{backend}".encode()
            proof = hmac.new(TOKEN.encode(), message, "sha256").hexdigest()
            body = json.dumps({
                "installation_proof": proof, "panel_managed": managed, "comfyui_url": backend,
                "asset_folders_proof": asset_folders_proof(
                    TOKEN.encode(), challenge.encode(), "input fixture", "output fixture",
                ),
            }).encode()
            self.server.proof_port = self.client_address[1]
            headers = {"Content-Type": "application/json"}
            if self.server.close_after_proof:
                headers["Connection"] = "close"
            self.reply(200, body, **headers)
            return
        self.server.calls.append("download")
        self.server.download_authorization = self.headers.get("Authorization")
        if self.server.redirect:
            self.reply(307, Location="/another-path")
            return
        self.reply(200, self.server.download, **{"Content-Type": "image/png"})

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.calls.append(request["method"])
        if self.server.proof_port is not None:
            assert self.client_address[1] == self.server.proof_port
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            self.send_error(401)
            return
        if self.server.redirect:
            self.reply(307, Location="/another-path")
            return
        method = request["method"]
        if method == "notifications/initialized":
            self.reply(202)
            return
        if method == "initialize":
            result = {"protocolVersion": "2025-03-26", "serverInfo": {"name": "test"}}
            sse = False
        else:
            assert method == "tools/call"
            assert request["params"] == {"name": "server_status", "arguments": {}}
            assert self.headers.get("Mcp-Session-Id") == "fixture-session"
            assert self.headers.get("MCP-Protocol-Version") == "2025-03-26"
            status = {"comfyui_running": self.server.comfyui_running}
            result = {"content": [{"type": "text", "text": json.dumps(status)}]}
            sse = True
        body = json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result})
        self.reply(200, (f"event: message\ndata: {body}\n\n" if sse else body).encode(), **{
            "Mcp-Session-Id": "fixture-session",
            "Content-Type": "text/event-stream" if sse else "application/json",
        })


class VerifyTest(unittest.TestCase):
    def setUp(self):
        # Respect workspace-local TEMP/TMP when exercising the CLI on Windows.
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.token = self.root / "token.txt"
        self.token.write_text(TOKEN + "\n", encoding="utf-8")
        self.claude = self.root / "claude"
        self.claude.mkdir()
        self.codex = self.root / "codex"
        self.codex.mkdir()
        server = ThreadingHTTPServer(("127.0.0.1", 0), BridgeHandler)
        server.calls = []
        server.redirect = False
        server.comfyui_running = True
        server.download = fixture_png()
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(thread.join, 5)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.server = server
        server.panel_managed = True
        server.close_after_proof = False
        server.proof_port = None
        self.url = f"http://127.0.0.1:{server.server_port}/mcp"

    def verify(self, *options, url=None, link=None):
        env = {
            **os.environ,
            "CLAUDE_CONFIG_DIR": str(self.claude),
            "CODEX_HOME": str(self.codex),
            "HTTP_PROXY": "http://127.0.0.1:1",
            "NO_PROXY": "",
        }
        response = subprocess.run(
            [sys.executable, "-B", str(SCRIPT), url or self.url, str(self.token), *options],
            capture_output=True,
            text=True,
            input=link,
            env=env,
            timeout=15,
        )
        self.assertNotIn(TOKEN, response.stdout + response.stderr)
        self.assertEqual(response.stderr, "")
        return json.loads(response.stdout)

    def signed_png_link(self, filename=f"{RENDER_PREFIX}_00001_.png", *, token=TOKEN, expires=None):
        expires = int(time.time()) + 120 if expires is None else expires
        message = f"output|mcp|{filename}|{expires}".encode()
        signature = hmac.new(token.encode(), message, hashlib.sha256).hexdigest()[:40]
        query = urlencode({"subfolder": "mcp", "type": "output", "exp": expires, "sig": signature})
        return self.url.removesuffix("/mcp") + f"/files/{filename}?{query}"

    def test_checks_the_signed_png_without_sending_the_token(self):
        self.assertEqual(self.verify("--first-render", RENDER_PREFIX, link=self.signed_png_link()),
                         {"first_render": "ok"})
        self.assertEqual(self.server.calls, ["download"])
        self.assertIsNone(self.server.download_authorization)

    def test_rejects_receipts_for_other_sessions_or_installations_without_requesting_them(self):
        links = [
            self.signed_png_link(filename="existing_00001_.png"),
            self.signed_png_link().replace("127.0.0.1", "example.invalid"),
            self.signed_png_link().replace("127.0.0.1", "127.0.0.2"),
            self.signed_png_link().replace("/files/", "/different-route/"),
            self.signed_png_link(token="other_test_installation_token_1234567890"),
            self.signed_png_link(expires=int(time.time()) - 180),
            self.signed_png_link() + "&type=output",
        ]
        for i, link in enumerate(links):
            with self.subTest(case=i):
                self.assertEqual(self.verify("--first-render", RENDER_PREFIX, link=link),
                                 {"first_render": "wrong_link"})
        self.assertEqual(self.server.calls, [])

    def test_refuses_a_redirect_from_a_valid_signed_link(self):
        self.server.redirect = True
        self.assertEqual(self.verify("--first-render", RENDER_PREFIX, link=self.signed_png_link()),
                         {"first_render": "download_failed"})
        self.assertEqual(self.server.calls, ["download"])

    def test_looks_at_png_size_and_content(self):
        for result in (fixture_png(16, 32), fixture_png()[:-16], b"this is not a PNG"):
            with self.subTest(size=len(result)):
                self.server.download = result
                self.assertEqual(self.verify("--first-render", RENDER_PREFIX, link=self.signed_png_link()),
                                 {"first_render": "invalid_png"})

    def claude_entry(self, bearer=TOKEN, url=None):
        return {
            "type": "http", "url": url or self.url,
            "headers": {"Authorization": f"Bearer {bearer}"},
        }

    def write_claude(self, **entries):
        (self.claude / ".claude.json").write_text(json.dumps({"mcpServers": entries}), encoding="utf-8")

    def write_codex(self, filename="config.toml", bearer=TOKEN, extra=""):
        (self.codex / filename).write_text(
            f'[mcp_servers.comfyui]\nurl = "{self.url}"\n{extra}'
            f'http_headers = {{ Authorization = "Bearer {bearer}" }}\n',
            encoding="utf-8",
        )

    def test_calls_read_only_status_and_checks_both_selected_config_files(self):
        self.write_claude(comfyui=self.claude_entry())
        self.write_codex()
        self.assertEqual(self.verify("claude", "codex"), {
            "bridge": "ok", "claude": "config_matches", "codex": "config_matches",
        })
        self.assertEqual(self.server.calls, ["initialize", "notifications/initialized", "tools/call"])

    def test_proves_the_listener_before_sending_its_private_token(self):
        self.assertEqual(self.verify("--require-owner"), {"bridge": "ok", "panel_managed": True})
        self.assertEqual(self.server.calls, ["owner", "initialize", "notifications/initialized", "tools/call"])

    def test_proves_a_manually_started_listener_separately(self):
        self.server.panel_managed = False
        self.assertEqual(self.verify("--require-owner"), {"bridge": "ok", "panel_managed": False})

    def test_does_not_send_the_token_over_a_new_connection_after_the_proof(self):
        self.server.close_after_proof = True
        self.assertEqual(self.verify("--require-owner"), {"bridge": "owner_unverified"})
        self.assertEqual(self.server.calls, ["owner"])

    def test_does_not_send_the_token_if_the_proven_backend_differs(self):
        self.assertEqual(self.verify("--require-owner", "--expected-backend", "http://127.0.0.1:8189"),
                         {"bridge": "backend_mismatch"})
        self.assertEqual(self.server.calls, ["owner"])

    def test_does_not_send_the_token_if_the_proven_asset_folders_differ(self):
        self.assertEqual(self.verify("--require-owner", "--expected-folders", "input fixture", "other output"),
                         {"bridge": "backend_mismatch"})
        self.assertEqual(self.server.calls, ["owner"])

    def test_accepts_matching_backend_address_and_folders(self):
        self.assertEqual(self.verify("--require-owner", "--expected-backend", "http://127.0.0.1:8188",
                                     "--expected-folders", "input fixture", "output fixture"),
                         {"bridge": "ok", "panel_managed": True})

    def test_does_not_send_the_token_to_a_different_installation(self):
        self.token.write_text("a_different_installation_token_with_more_than_32_chars", encoding="utf-8")
        self.assertEqual(self.verify("--require-owner"), {"bridge": "owner_unverified"})
        self.assertEqual(self.server.calls, ["owner"])

    def test_handles_failed_backend_and_client_mismatch_separately(self):
        self.server.comfyui_running = False
        self.write_claude(comfyui=self.claude_entry(bearer="a different token"))
        self.write_codex(extra="enabled = false\n")
        self.assertEqual(self.verify("claude", "codex"), {
            "bridge": "comfyui_unavailable", "claude": "token_mismatch", "codex": "server_disabled",
        })

    def test_reports_missing_or_unreadable_config(self):
        (self.codex / "config.toml").write_text("INVALID [TOML", encoding="utf-8")
        self.assertEqual(self.verify("claude", "codex"), {
            "bridge": "ok", "claude": "config_missing", "codex": "config_unreadable",
        })

    def test_checks_explicit_codex_profile_and_detects_conflicting_layer(self):
        self.write_codex("studio.config.toml")
        self.assertEqual(self.verify("codex", "--codex-profile", "studio")["codex"], "config_matches")
        self.write_codex(bearer="previous token")
        self.assertEqual(self.verify("codex", "--codex-profile", "studio")["codex"], "config_conflict")

    def test_checks_alternate_name_if_exact_url_is_unique(self):
        self.write_claude(alternate=self.claude_entry())
        self.assertEqual(self.verify("claude")["claude"], "config_matches")
        self.write_claude(alternate=self.claude_entry(), second=self.claude_entry())
        self.assertEqual(self.verify("claude")["claude"], "config_conflict")

    def test_finds_exact_url_when_default_name_points_to_a_different_server(self):
        self.write_claude(comfyui=self.claude_entry(url="http://127.0.0.1:1/mcp"), extra=self.claude_entry())
        self.assertEqual(self.verify("claude")["claude"], "config_matches")

    def test_explicit_name_checks_only_that_entry(self):
        self.write_claude(comfyui=self.claude_entry(url="http://127.0.0.1:1/mcp"), extra=self.claude_entry())
        self.assertEqual(self.verify("claude", "--server-name", "comfyui")["claude"], "url_mismatch")
        self.assertEqual(self.verify("claude", "--server-name", "absent")["claude"], "server_missing")

    def test_prefers_default_name_when_its_url_matches(self):
        self.write_claude(comfyui=self.claude_entry(), extra=self.claude_entry(bearer="old token"))
        self.assertEqual(self.verify("claude")["claude"], "config_matches")

    def test_reports_dynamic_auth_as_unverified(self):
        self.write_claude(comfyui=self.claude_entry(bearer="${RENDER_BRIDGE_MCP_TOKEN}"))
        self.assertEqual(self.verify("claude")["claude"], "auth_unverified")

    def test_rejects_non_loopback_before_any_mcp_call(self):
        self.assertEqual(self.verify(url="http://example.invalid:8189/mcp"), {"bridge": "invalid_url"})
        self.assertEqual(self.server.calls, [])

    def test_never_follows_redirects(self):
        self.server.redirect = True
        self.assertEqual(self.verify(), {"bridge": "unexpected_response"})
        self.assertEqual(self.server.calls, ["initialize"])

    def test_reports_rejected_token_before_calling_status(self):
        self.token.write_text("another_test_token_that_will_never_be_used_for_auth", encoding="utf-8")
        self.assertEqual(self.verify(), {"bridge": "unauthorized"})
        self.assertEqual(self.server.calls, ["initialize"])


if __name__ == "__main__":
    unittest.main()
