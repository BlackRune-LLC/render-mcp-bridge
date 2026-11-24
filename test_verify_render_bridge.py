import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


SCRIPT = Path(__file__).with_name("verify_render_bridge.py")
TOKEN = "a_test_token_that_will_never_be_used_for_auth"


class BridgeHandler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.calls.append(request["method"])
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            self.send_error(401)
            return
        if self.server.redirect:
            self.send_response(307)
            self.send_header("Location", "/another-path")
            self.end_headers()
            return
        method = request["method"]
        if method == "notifications/initialized":
            self.send_response(202)
            self.end_headers()
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
        self.send_response(200)
        self.send_header("Mcp-Session-Id", "fixture-session")
        self.send_header("Content-Type", "text/event-stream" if sse else "application/json")
        self.end_headers()
        self.wfile.write((f"event: message\ndata: {body}\n\n" if sse else body).encode())


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
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(thread.join, 5)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.server = server
        self.url = f"http://127.0.0.1:{server.server_port}/mcp"

    def verify(self, *options, url=None):
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
            env=env,
            timeout=15,
        )
        self.assertNotIn(TOKEN, response.stdout + response.stderr)
        self.assertEqual(response.stderr, "")
        return json.loads(response.stdout)

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
