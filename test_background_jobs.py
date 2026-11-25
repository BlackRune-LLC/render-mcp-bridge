import asyncio
import hmac
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

    async def test_gaming_mode_unloads_once_after_the_queue_becomes_empty(self):
        backend = AsyncMock()
        backend.get.side_effect = [
            response(queue_running=[["test job"]], queue_pending=[]),
            response(queue_running=[], queue_pending=[["next job"]]),
            response(queue_running=[], queue_pending=[]),
        ]
        backend.post.return_value = response()
        with patch.object(server, "_http", backend):
            await server.enable_gaming_mode()
            await server.idle_check()
            backend.post.assert_not_awaited()
            await server.idle_check()
            await server.idle_check()
        backend.post.assert_awaited_once_with(
            "/free", json={"unload_models": True, "free_memory": True}, timeout=10,
        )
        self.assertFalse(self.state["models_loaded"])
        self.assertEqual(self.state["last_unload_reason"], "gaming mode")
        self.assertTrue(server.PAUSE_FLAG.exists())

    async def test_gaming_mode_rechecks_after_unavailable_or_invalid_queue(self):
        server.set_gaming_mode(True)
        backend = AsyncMock()
        backend.get.side_effect = [
            httpx.ConnectError("offline"),
            response(queue_running=[]),
            response(queue_running=[], queue_pending=[]),
            response(queue_running=[], queue_pending=[]),
        ]
        backend.post.side_effect = [response(503), response()]
        with patch.object(server, "_http", backend):
            await server.idle_check()
            await server.idle_check()
            backend.post.assert_not_awaited()
            await server.idle_check()
            self.assertTrue(self.state["models_loaded"])
            await server.idle_check()
            await server.idle_check()
        self.assertEqual(backend.post.await_count, 2)
        self.assertFalse(self.state["models_loaded"])

    async def test_gaming_mode_change_during_queue_check_requires_a_new_check(self):
        for enabled_again in (False, True):
            with self.subTest(enabled_again=enabled_again):
                server.set_gaming_mode(True)
                backend = AsyncMock()
                empty_queue = response(queue_running=[], queue_pending=[])

                async def change_gaming_mode(*args, **kwargs):
                    server.set_gaming_mode(False)
                    if enabled_again:
                        server.set_gaming_mode(True)
                    return empty_queue

                backend.get.side_effect = change_gaming_mode
                backend.post.return_value = response()
                with patch.object(server, "_http", backend):
                    await server.idle_check()
                    backend.get.assert_awaited_once()
                    backend.post.assert_not_awaited()
                    if enabled_again:
                        backend.get.side_effect = None
                        backend.get.return_value = empty_queue
                        await server.idle_check()
                        backend.post.assert_awaited_once()

    async def test_gaming_mode_cli_and_watcher_share_the_same_activation(self):
        backend = AsyncMock()
        backend.get.return_value = response(queue_running=[], queue_pending=[])
        backend.post.return_value = response()
        with patch.object(server, "_http", backend):
            await server.enable_gaming_mode()
            with patch.object(server, "_state", {**self.state, "models_loaded": True, "last_unload": None}):
                await server.idle_check()
                self.assertFalse(server._state["models_loaded"])
            await server.enable_gaming_mode()
            backend.post.assert_awaited_once()
            server.set_gaming_mode(False)
            await server.enable_gaming_mode()
        self.assertEqual(backend.post.await_count, 2)

    async def test_normal_idle_unload_defers_when_gaming_mode_starts_during_probe(self):
        backend = AsyncMock()

        async def check_backend(path, **kwargs):
            if path == "/queue":
                return response(queue_running=[], queue_pending=[])
            server.set_gaming_mode(True)
            return response(last={})

        backend.get.side_effect = check_backend
        backend.post.return_value = response()
        with patch.object(server, "_http", backend):
            await server.idle_check()
        backend.post.assert_not_awaited()

    async def test_gaming_mode_does_not_repeat_an_unload_while_cli_checks_queue(self):
        checking = asyncio.Event()
        reply = asyncio.Event()
        backend = AsyncMock()
        backend.post.return_value = response()

        async def check_queue(*args, **kwargs):
            checking.set()
            await reply.wait()
            return response(queue_running=[], queue_pending=[])

        backend.get.side_effect = check_queue
        with patch.object(server, "_http", backend):
            cli = asyncio.create_task(server.enable_gaming_mode())
            try:
                await checking.wait()
                await asyncio.wait_for(server.idle_check(), timeout=1)
            finally:
                reply.set()
                await cli
            await server.idle_check()
        backend.post.assert_awaited_once()

    async def test_health_proof_is_bound_to_a_valid_per_request_challenge(self):
        key = "test_installation_token_fixture_0123456789"
        challenge = "ab" * 16
        message = f"rb-health-v2:{challenge}:0:{server.COMFY_URL}".encode()
        expected = hmac.new(key.encode(), message, "sha256").hexdigest()
        transport = httpx.ASGITransport(app=server.BearerAuth(None, key))
        async with httpx.AsyncClient(transport=transport, base_url="http://127.0.0.1") as client:
            proof = await client.get("/health", headers={"X-Render-Bridge-Challenge": challenge})
            invalid = await client.get("/health", headers={"X-Render-Bridge-Challenge": "invalid"})
        self.assertEqual(proof.json()["installation_proof"], expected)
        self.assertIs(proof.json()["panel_managed"], False)
        self.assertNotIn("installation_proof", invalid.json())
        self.assertNotIn("comfyui_url", invalid.json())
        self.assertNotIn(str(server.COMFY_DIRS["output"]), proof.text)
        self.assertNotIn(key, proof.text)

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


class MediaOutputTests(unittest.IsolatedAsyncioTestCase):
    async def test_generate_image_requires_the_primary_png(self):
        queue = AsyncMock(return_value=("test-prompt", {
            "outputs": {"original": {"images": [{"filename": "secondary.png"}]}},
        }, 1.2))
        with (
            patch.object(server, "use_project", return_value=(None, None)),
            patch.object(server, "resolve_preset", AsyncMock(return_value={
                "size": (512, 512), "steps": 9, "cfg": 1, "family": "z_image", "negative": "",
            })),
            patch.object(server, "build_graph", return_value={}),
            patch.object(server, "queue_and_wait", queue),
            patch.object(server, "log_asset") as log,
        ):
            with self.assertRaisesRegex(server.ToolError, "without a saved PNG"):
                await server.generate_image("A simple fixture", preview=False)
        self.assertTrue(log.call_args.args[1]["incomplete"])
        self.assertEqual(log.call_args.args[2][0]["filename"], "secondary.png")

    async def test_animation_records_partial_frames_and_fails_when_mp4_is_missing(self):
        entry = {"outputs": {"save1": {"images": [{"filename": "frame.png"}]}}}
        with patch.object(server, "log_asset") as log:
            with self.assertRaisesRegex(server.ToolError, "without a saved MP4"):
                await server._frames_result("animate_loop", {}, entry, ["save1"], preview=False)
        self.assertTrue(log.call_args.args[1]["incomplete"])
        self.assertEqual(log.call_args.args[2][0]["filename"], "frame.png")

    async def test_image_to_3d_requires_a_glb_from_the_model_save_node(self):
        queue = AsyncMock(return_value=("test-prompt", {
            "outputs": {"save": {"images": [{"filename": "preview.png"}]}},
        }, 1.2))
        with (
            patch.object(server, "use_project"),
            patch.object(server, "load_3d_workflow", return_value={
                "save": {"class_type": "Save3DAdvanced", "inputs": {"viewport_state": "{}"}},
            }),
            patch.object(server, "input_image_ref", AsyncMock(return_value="fixture.png")),
            patch.object(server, "queue_and_wait", queue),
        ):
            with self.assertRaisesRegex(server.ToolError, "without a saved GLB"):
                await server.image_to_3d(filename="fixture.png")

    async def test_model_list_explains_an_invalid_3d_workflow_file(self):
        with (
            patch.object(server, "THREE_D_WORKFLOW", "unreadable-workflow.json"),
            patch.object(server, "ensure_comfy", AsyncMock()),
            patch.object(server, "model_lists", AsyncMock(return_value={})),
            patch.object(server, "resolve_preset", AsyncMock()),
            patch.object(server, "_http", AsyncMock()),
        ):
            server._http.get.return_value = response(devices=[{}])
            status = json.loads(await server.list_models())
        self.assertFalse(status["image_to_3d_configured"])
        self.assertIn("Save3DAdvanced", status["image_to_3d_error"])


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
