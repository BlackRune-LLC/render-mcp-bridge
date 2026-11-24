"""Exercise signed downloads against an in-memory ComfyUI response stream."""

import asyncio
import contextlib
import hashlib
import hmac
import io
import socket
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.parse import urlencode

import httpx
from starlette.requests import ClientDisconnect

import comfy_mcp_server as bridge


FIXTURE_TOKEN = "signed-download-test-token-only"


class TokenRotationTests(unittest.TestCase):
    def test_requires_a_stopped_server_then_changes_the_token_and_signed_links(self):
        with tempfile.TemporaryDirectory() as directory:
            token_file = Path(directory) / "token.txt"
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                listener.listen(1)
                port = listener.getsockname()[1]
                with (
                    patch.object(bridge, "TOKEN_FILE", token_file),
                    patch.object(bridge, "MCP_HOSTS", ["127.0.0.1"]),
                    patch.object(bridge, "MCP_PORT", port),
                    contextlib.redirect_stdout(io.StringIO()),
                ):
                    bridge.load_token.cache_clear()
                    self.addCleanup(bridge.load_token.cache_clear)
                    before = bridge.load_token()
                    before_link = bridge._link_sig("fixture.png", "", "output", 42)
                    with self.assertRaises(SystemExit):
                        bridge.rotate_token()
                    self.assertEqual(token_file.read_text().strip(), before)
                    listener.close()
                    bridge.rotate_token()
                    bridge.load_token.cache_clear()
                    self.assertNotEqual(bridge.load_token(), before)
                    self.assertNotEqual(bridge._link_sig("fixture.png", "", "output", 42), before_link)


class FixtureStream(httpx.AsyncByteStream):
    def __init__(self, events, chunks):
        self.events = events
        self.chunks = chunks

    async def __aiter__(self):
        for number, chunk in enumerate(self.chunks):
            self.events.append(("upstream-body", number))
            yield chunk

    async def aclose(self):
        self.events.append(("upstream-closed", None))


class SignedDownloadTests(unittest.IsolatedAsyncioTestCase):
    async def download(self, stream, *, method="GET", status=200, headers=None,
                       valid_signature=True, send=None, receive=None, spec_version="2.4"):
        expiration = int(time.time()) + 60
        payload = f"output|preview|sample clip.mp4|{expiration}".encode()
        signature = hmac.new(FIXTURE_TOKEN.encode(), payload, hashlib.sha256).hexdigest()[:40]
        query = urlencode({
            "exp": expiration, "subfolder": "preview", "type": "output",
            "sig": signature if valid_signature else "invalid",
        })
        scope = {
            "type": "http", "asgi": {"spec_version": spec_version}, "method": method,
            "path": "/files/sample clip.mp4", "query_string": query.encode(),
        }

        async def upstream(request):
            stream.events.append(("upstream-method", request.method))
            return httpx.Response(status, headers=headers, stream=stream)

        async def capture(message):
            stream.events.append(("downstream", message))

        async def reject_receive():
            raise AssertionError("The protocol detects disconnect through send in ASGI 2.4.")

        async with httpx.AsyncClient(base_url="http://127.0.0.1", transport=httpx.MockTransport(upstream)) as backend:
            with patch.object(bridge, "_http", backend), patch.object(bridge, "load_token", return_value=FIXTURE_TOKEN):
                await bridge.serve_signed_file(scope, receive or reject_receive, send or capture)
        return [message for kind, message in stream.events if kind == "downstream"]

    async def test_sends_first_chunk_before_reading_the_rest(self):
        events = []
        chunks = [b"a" * 65536, b"b" * 131072, b"c"]
        messages = await self.download(FixtureStream(events, chunks), headers={
            "Content-Type": "video/mp4", "Content-Length": "196609",
        })

        start, *body = messages
        self.assertEqual(start["status"], 200)
        self.assertEqual(dict(start["headers"])[b"content-type"], b"video/mp4")
        self.assertEqual(dict(start["headers"])[b"content-length"], b"196609")
        self.assertEqual(dict(start["headers"])[b"content-disposition"], b'attachment; filename="sample%20clip.mp4"')
        self.assertEqual(b"".join(message.get("body", b"") for message in body), b"".join(chunks))
        self.assertLess(events.index(("downstream", body[0])), events.index(("upstream-body", 1)))
        self.assertLessEqual(max(len(message.get("body", b"")) for message in body), 65536)
        self.assertIn(("upstream-closed", None), events)

    async def test_head_preserves_metadata_without_reading_the_file(self):
        events = []
        messages = await self.download(FixtureStream(events, [b"unused body"]), method="HEAD", headers={
            "Content-Type": "video/mp4", "Content-Length": "196609",
        })

        self.assertIn(("upstream-method", "HEAD"), events)
        self.assertNotIn(("upstream-body", 0), events)
        self.assertIn(("upstream-closed", None), events)
        self.assertEqual(messages[0]["status"], 200)
        self.assertEqual(dict(messages[0]["headers"])[b"content-length"], b"196609")
        self.assertEqual([message.get("body", b"") for message in messages[1:]], [b""])

    async def test_invalid_signature_never_requests_upstream(self):
        events = []
        messages = await self.download(FixtureStream(events, []), valid_signature=False)
        self.assertEqual(messages[0]["status"], 403)
        self.assertTrue(all(kind == "downstream" for kind, _ in events))

    async def test_missing_file_closes_upstream_without_reading_error_body(self):
        events = []
        messages = await self.download(FixtureStream(events, [b"unused upstream error"]), status=404)
        self.assertEqual(messages[0]["status"], 404)
        self.assertNotIn(("upstream-body", 0), events)
        self.assertIn(("upstream-closed", None), events)

    async def test_client_disconnect_closes_upstream_without_reading_rest(self):
        events = []

        async def send(message):
            if message["type"] == "http.response.body":
                raise OSError("fixture disconnected")

        with self.assertRaises(ClientDisconnect):
            await self.download(FixtureStream(events, [b"a" * 65536, b"unread tail"]), send=send)
        self.assertNotIn(("upstream-body", 1), events)
        self.assertIn(("upstream-closed", None), events)

    async def test_uvicorn_disconnect_during_blocked_send_closes_upstream(self):
        events = []
        sending = asyncio.Event()

        async def send(message):
            if message["type"] == "http.response.body":
                sending.set()
                await asyncio.Future()

        async def receive():
            await sending.wait()
            return {"type": "http.disconnect"}

        async with asyncio.timeout(2):
            await self.download(FixtureStream(events, [b"a" * 65536, b"unread tail"]),
                                send=send, receive=receive, spec_version="2.3")
        self.assertNotIn(("upstream-body", 1), events)
        self.assertIn(("upstream-closed", None), events)
