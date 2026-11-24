import asyncio
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, patch

import httpx

import comfy_mcp_server as server


class RuntimeNetworkTests(unittest.TestCase):
    def test_manual_runtime_rejects_a_nonlocal_backend(self):
        result = subprocess.run(
            [sys.executable, "-B", server.__file__, "--help"],
            env={**os.environ, "COMFY_URL": "https://backend.example.test:8188"},
            capture_output=True, text=True, timeout=30,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Invalid COMFY_URL", result.stderr)
        self.assertNotIn("backend.example.test", result.stderr)

    def test_mcp_tracing_middleware_is_disabled(self):
        self.assertFalse(any(
            isinstance(middleware, server.OpenTelemetryMiddleware)
            for middleware in server.mcp._lowlevel_server.middleware
        ))


def response(status=200, **body):
    return httpx.Response(status, json=body, request=httpx.Request("GET", "http://127.0.0.1/"))


def job_record(**overrides):
    return {
        "id": "123456abcdef", "tool": "generate_image", "label": "test job",
        "status": "running", "created": time.time(), "prompt_ids": ["fixture-prompt"],
        **overrides,
    }


class ModelLifecycleTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.enterContext(patch.object(server, "PAUSE_FLAG", Path(directory.name) / "pause"))
        self.enterContext(patch.object(server, "IDLE_UNLOAD_S", 60))
        self.state = {**server._state, "history_seen": True, "last_history_id": "last",
                      "last_activity": time.time() - 120, "models_loaded": True}
        self.enterContext(patch.object(server, "_state", self.state))
        self.enterContext(patch.object(server, "comfy_up", AsyncMock(return_value=True)))

    async def test_idle_models_unload_only_after_a_verified_empty_queue(self):
        backend = AsyncMock()
        backend.get.side_effect = [response(queue_running=[], queue_pending=[]), response(last={})]
        backend.post.return_value = response()
        with (
            patch.object(server, "_http", backend),
            patch.object(server.asyncio, "sleep", AsyncMock()),
            patch.object(server, "gpu_info", AsyncMock(return_value={})),
        ):
            await server.idle_check()
        backend.post.assert_awaited_once_with(
            "/free", json={"unload_models": True, "free_memory": True}, timeout=10,
        )
        self.assertFalse(self.state["models_loaded"])
        self.assertTrue(self.state["last_unload_reason"].startswith("idle "))

    async def test_pending_jobs_or_unknown_queue_never_trigger_idle_unload(self):
        for queue in [response(queue_running=[], queue_pending=[{}]), response(state="unknown")]:
            with self.subTest(queue=queue.json()):
                backend = AsyncMock()
                backend.get.return_value = queue
                with patch.object(server, "_http", backend):
                    await server.idle_check()
                backend.post.assert_not_called()

    async def test_load_models_uses_a_preview_warmup_without_writing_an_asset(self):
        queue = AsyncMock(return_value=({}, [], 1.2))
        with (
            patch.object(server, "ensure_comfy", AsyncMock()),
            patch.object(server, "resolve_preset", AsyncMock(return_value={"cfg": 1})),
            patch.object(server, "build_graph", return_value={}),
            patch.object(server, "queue_and_wait", queue),
            patch.object(server, "gpu_info", AsyncMock(return_value={})),
        ):
            result = json.loads(await server.load_models())
        self.assertEqual(result["loaded_preset"], "z_image_turbo")
        self.assertEqual(queue.await_args.args[0]["save"]["class_type"], "PreviewImage")


class JobTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.enterContext(patch.object(server, "DATA_DIR", Path(directory.name)))
        self.enterContext(patch.object(server, "_jobs", {}))
        self.enterContext(patch.object(server, "signed_url", lambda *_: "http://127.0.0.1/fresh"))

    async def test_restart_restores_done_references_without_payloads(self):
        image = {"filename": "result.png", "subfolder": "mcp", "type": "output", "url": "keep-private"}

        async def generate(prompt):
            server._remember_job_outputs(server._current_job.get(), [image])
            return server.CallToolResult(content=[
                server.TextContent(type="text", text=json.dumps({"prompt": prompt, "images": [image]})),
            ])

        with patch.dict(server.JOB_TOOLS, {"generate_image": generate}):
            accepted = json.loads(await server.start_job("generate_image", {"prompt": "unpersisted prompt"}))
            job_id = accepted["job_id"]
            live = await server.get_job(job_id, wait_seconds=5)
        self.assertEqual(json.loads(live.content[0].text)["status"], "done")
        metadata = next(Path(server.DATA_DIR).rglob("*.json")).read_text(encoding="utf-8")
        self.assertNotIn("unpersisted prompt", metadata)
        self.assertNotIn("keep-private", metadata)
        server._jobs.clear()
        server._restore_jobs()
        restored = json.loads((await server.get_job(job_id)).content[0].text)
        self.assertEqual(restored["status"], "done")
        self.assertEqual(restored["outputs"][0]["url"], "http://127.0.0.1/fresh")

    async def test_cancel_does_not_claim_success_when_backend_is_offline(self):
        job = job_record()
        server._jobs[job["id"]] = job
        with patch.object(server._http, "get", side_effect=httpx.ConnectError("offline")):
            result = json.loads(await server.cancel_job(job["id"]))
        self.assertEqual(result["status"], "cancel_unknown")

    async def test_cancel_pending_confirms_removal_without_touching_other_jobs(self):
        job = job_record()
        server._jobs[job["id"]] = job
        with (
            patch.object(server._http, "get", side_effect=[
                response(queue_running=[[1, "unrelated"]], queue_pending=[[2, "fixture-prompt"]]),
                response(queue_running=[[1, "unrelated"]], queue_pending=[]),
                response(),
            ]),
            patch.object(server._http, "post", return_value=response(cancelled=True)) as post,
        ):
            result = json.loads(await server.cancel_job(job["id"]))
        post.assert_awaited_once_with("/api/jobs/fixture-prompt/cancel", timeout=5)
        self.assertEqual(result["status"], "cancelled")

    async def test_running_interrupt_waits_for_upstream_confirmation(self):
        job = job_record()
        server._jobs[job["id"]] = job
        active = response(queue_running=[[1, "fixture-prompt"]], queue_pending=[])
        finished = response(queue_running=[], queue_pending=[])
        interrupted = response(**{"fixture-prompt": {
            "status": {"status_str": "error", "messages": [["execution_interrupted", {}]]},
        }})
        with (
            patch.object(server._http, "get", side_effect=[active, active, finished, interrupted]),
            patch.object(server._http, "post", return_value=response(cancelled=True)) as post,
        ):
            result = json.loads(await server.cancel_job(job["id"]))
            self.assertEqual(result["status"], "cancel_requested")
            restored = json.loads((await server.get_job(job["id"])).content[0].text)
        self.assertEqual(restored["status"], "cancelled")
        post.assert_awaited_once_with("/api/jobs/fixture-prompt/cancel", timeout=5)

    async def test_older_comfy_does_not_interrupt_unrelated_running_work(self):
        job = job_record()
        server._jobs[job["id"]] = job
        active = response(queue_running=[[1, "fixture-prompt"]], queue_pending=[])
        with (
            patch.object(server._http, "get", return_value=active),
            patch.object(server._http, "post", return_value=response(404)) as post,
        ):
            result = json.loads(await server.cancel_job(job["id"]))
        self.assertEqual(result["status"], "cancel_unknown")
        post.assert_awaited_once_with("/api/jobs/fixture-prompt/cancel", timeout=5)

    async def test_queued_prompt_that_started_before_deletion_stays_uncertain(self):
        job = job_record()
        server._jobs[job["id"]] = job
        with (
            patch.object(server._http, "get", side_effect=[
                response(queue_running=[], queue_pending=[[1, "fixture-prompt"]]),
                response(queue_running=[[1, "fixture-prompt"]], queue_pending=[]),
            ]),
            patch.object(server._http, "post", side_effect=[response(404), response()]) as post,
        ):
            result = json.loads(await server.cancel_job(job["id"]))
        self.assertEqual(result["status"], "cancel_unknown")
        self.assertEqual(post.await_count, 2)
        post.assert_awaited_with("/queue", json={"delete": ["fixture-prompt"]}, timeout=5)

    async def test_completion_before_cancellation_recovers_files_without_false_success(self):
        job = job_record()
        server._jobs[job["id"]] = job
        empty_queue = response(queue_running=[], queue_pending=[])
        completed = response(**{"fixture-prompt": {
            "status": {"status_str": "success", "completed": True},
            "outputs": {"out": {"images": [{"filename": "already-done.png"}]}},
        }})
        with (
            patch.object(server._http, "get", side_effect=[empty_queue, empty_queue, completed]),
            patch.object(server._http, "post") as post,
        ):
            result = json.loads(await server.cancel_job(job["id"]))
            recovered = json.loads((await server.get_job(job["id"])).content[0].text)
        self.assertEqual(result["status"], "interrupted")
        self.assertEqual(recovered["outputs"][0]["filename"], "already-done.png")
        post.assert_not_awaited()

    async def test_changed_storage_does_not_restore_another_comfy_builds_jobs(self):
        job = job_record()
        server._remember_job(job)
        other = Path(server.DATA_DIR) / "different-comfy-build"
        with patch.dict(server.COMFY_DIRS, {"input": other / "input", "output": other / "output"}):
            server._restore_jobs()
        self.assertEqual(server._jobs, {})

    async def test_restart_recovers_outputs_without_resubmitting(self):
        job = job_record()
        server._remember_job(job)
        server._restore_jobs()
        active = response(queue_running=[[1, "fixture-prompt"]], queue_pending=[])
        finished = response(queue_running=[], queue_pending=[])
        history = response(**{"fixture-prompt": {
            "status": {"status_str": "success", "completed": True},
            "outputs": {"out": {"images": [{"filename": "partial.png", "type": "output"}]}},
        }})
        with (
            patch.object(server._http, "get", side_effect=[active, finished, history]),
            patch.object(server._http, "post") as post,
        ):
            active_job = json.loads((await server.get_job(job["id"])).content[0].text)
            result = json.loads((await server.get_job(job["id"])).content[0].text)
        self.assertEqual(active_job["status"], "recovering")
        self.assertEqual(result["status"], "interrupted")
        self.assertEqual(result["outputs"][0]["filename"], "partial.png")
        post.assert_not_awaited()

    async def test_unacknowledged_submission_cannot_be_confirmed_cancelled(self):
        job = job_record(prompt_ids=[], submission_pending=True)
        server._jobs[job["id"]] = job
        result = json.loads(await server.cancel_job(job["id"]))
        self.assertEqual(result["status"], "cancel_unknown")

    async def test_read_prunes_expired_jobs_without_another_submission(self):
        expired = job_record(status="done", finished=time.time() - server.JOB_RETENTION_S - 10)
        server._jobs[expired["id"]] = expired
        server._remember_job(expired)
        self.assertEqual(json.loads(await server.list_jobs()), [])
        self.assertEqual(list(Path(server.DATA_DIR).rglob("*.json")), [])


if __name__ == "__main__":
    unittest.main()
