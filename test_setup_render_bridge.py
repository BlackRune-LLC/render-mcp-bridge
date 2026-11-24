import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import setup_render_bridge as setup


class InstallRuntimeTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.data = Path(temporary.name)
        self.settings = {"COMFY_ROOT": str(self.data / "comfy"), "COMFY_URL": "http://localhost:8188"}
        self.previous = self.data / "app-Previous"
        self.previous.mkdir()
        setup.save_settings(self.previous / "settings.json", self.settings)
        setup.atomic_text(self.data / setup.ACTIVE_RUNTIME, self.previous.name)
        (self.data / "token.txt").write_text("fixture-token", encoding="utf-8")

    def test_failed_upgrade_preserves_the_previous_runtime_and_removes_staging(self):
        failure = setup.InstallFailure("Dependency installation failed")
        with patch.object(setup, "run_install_stage", side_effect=[None, failure]), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(setup.InstallFailure):
                setup.install_runtime(self.data, {**self.settings, "COMFY_URL": "http://localhost:9288"})
        self.assertEqual(setup.active_runtime(self.data), self.previous)
        self.assertEqual(list(self.data.glob("app-*")), [self.previous])
        self.assertEqual(setup.load_settings(self.previous / "settings.json"), self.settings)
        self.assertEqual((self.data / "token.txt").read_text(encoding="utf-8"), "fixture-token")

    def test_unexpected_installer_crash_reports_only_safe_fields_in_the_default_location(self):
        local = self.data / "local profile"
        custom = self.data / "custom runtime"
        custom.mkdir()
        custom.joinpath("runtime-marker").write_text("retain", encoding="utf-8")
        override = {"LOCALAPPDATA": str(local), "XDG_DATA_HOME": str(local), "COMFY_MCP_DATA_DIR": str(custom)}
        message = f"fixture confidential token and absolute folder {custom}"
        logs = local / "comfy-mcp" / "crash-logs"
        with patch.dict(os.environ, override), contextlib.redirect_stderr(io.StringIO()) as stderr:
            setup.record_crash("server", RuntimeError(message))
            original_server_log = (logs / "server.json").read_bytes()
            with patch.object(setup, "install", side_effect=RuntimeError(message)):
                self.assertEqual(setup.main(["--data-dir", str(custom), "install"]), 1)
            self.assertIn("A local file or process could not be accessed.", stderr.getvalue())
            log = logs / "terminal-installer.json"
            original = log.read_text(encoding="utf-8")
            with patch.object(setup, "install", side_effect=EOFError()):
                self.assertEqual(setup.main(["--data-dir", str(custom), "install"]), 130)
            self.assertEqual(log.read_text(encoding="utf-8"), original)

        report = json.loads(original)
        self.assertEqual(set(report), {"schema", "timestamp", "component", "python_version", "exception_type", "stack"})
        self.assertEqual((report["schema"], report["component"], report["exception_type"]), (1, "terminal-installer", "RuntimeError"))
        self.assertRegex(report["timestamp"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        self.assertEqual(len(report["stack"]), 1)
        self.assertEqual(report["stack"][0]["file"], "setup_render_bridge.py")
        self.assertEqual(report["stack"][0]["function"], "main")
        self.assertIsInstance(report["stack"][0]["line"], int)
        self.assertNotIn("fixture confidential token", original)
        self.assertNotIn("custom runtime", original)
        self.assertEqual((logs / "server.json").read_bytes(), original_server_log)
        self.assertFalse((custom / "crash-logs").exists())
        self.assertEqual((custom / "runtime-marker").read_text(encoding="utf-8"), "retain")

    def test_verified_upgrade_switches_the_pointer_and_keeps_copied_launchers_usable(self):
        updated = {**self.settings, "COMFY_URL": "http://localhost:9288"}
        with patch.object(setup, "run_install_stage") as stages, contextlib.redirect_stdout(io.StringIO()):
            setup.install_runtime(self.data, updated)
        active = setup.active_runtime(self.data)
        self.assertNotEqual(active, self.previous)
        self.assertEqual(stages.call_count, 4)
        self.assertEqual(setup.load_settings(active / "settings.json"), updated)
        self.assertEqual(setup.load_settings(self.previous / "settings.json"), self.settings)
        python = setup.venv_python(active)
        python.parent.mkdir(parents=True)
        python.touch()
        with patch.object(setup, "PROJECT_ROOT", self.previous), patch.object(setup.subprocess, "run", return_value=Mock(returncode=0)) as run:
            self.assertEqual(setup.launch(self.data / "settings.json", ["gaming-mode", "status"]), 0)
        self.assertEqual(run.call_args.args[0][0], str(python))
        self.assertEqual(run.call_args.kwargs["env"]["COMFY_URL"], updated["COMFY_URL"])
        self.assertEqual(run.call_args.kwargs["env"]["COMFY_MCP_DATA_DIR"], str(self.data))

        previous_python = setup.venv_python(self.previous)
        previous_python.parent.mkdir(parents=True)
        previous_python.touch()
        for folder in ("input", "output"):
            (Path(self.settings["COMFY_ROOT"]) / folder).mkdir(parents=True)
        with patch.object(setup, "PROJECT_ROOT", self.previous), patch.object(setup.subprocess, "run", return_value=Mock(returncode=0)) as run:
            self.assertEqual(setup.launch(self.data / "settings.json", []), 0)
        self.assertEqual(run.call_args.args[0][0], str(previous_python))
        self.assertEqual(run.call_args.kwargs["env"]["COMFY_URL"], self.settings["COMFY_URL"])

    def test_invalid_pointer_cannot_redirect_outside_installation(self):
        setup.atomic_text(self.data / setup.ACTIVE_RUNTIME, "app-../../escape")
        with self.assertRaises(ValueError):
            setup.active_runtime(self.data)

    def test_launch_uses_the_saved_install_port_instead_of_the_parent_environment(self):
        python = setup.venv_python(self.previous)
        python.parent.mkdir(parents=True)
        python.touch()
        (self.data / "mcp-port").write_text("19189\n", encoding="utf-8")
        for inherited in ({}, {"MCP_PORT": "8188"}):
            with (
                patch.dict(os.environ, inherited, clear=True),
                patch.object(setup.subprocess, "run", return_value=Mock(returncode=0)) as run,
            ):
                self.assertEqual(setup.launch(self.data / "settings.json", ["rotate-token"]), 0)
                self.assertEqual(run.call_args.kwargs["env"]["MCP_PORT"], "19189")

    def test_terminal_install_keeps_a_custom_port_across_later_installs(self):
        with (
            patch.dict(os.environ, {"MCP_PORT": "19189"}),
            patch.object(setup, "run_install_stage"),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            setup.install_runtime(self.data, self.settings)
            self.assertEqual((self.data / "mcp-port").read_text(encoding="utf-8").strip(), "19189")
            with patch.dict(os.environ, {"MCP_PORT": "8188"}):
                setup.install_runtime(self.data, self.settings)
            self.assertEqual((self.data / "mcp-port").read_text(encoding="utf-8").strip(), "19189")

    def test_unchanged_root_keeps_custom_input_and_output_folders(self):
        inputs = self.data / "custom-input"
        outputs = self.data / "custom-output"
        inputs.mkdir()
        outputs.mkdir()
        saved = {**self.settings, "COMFY_INPUT_DIR": str(inputs), "COMFY_OUTPUT_DIR": str(outputs)}
        root = setup.comfy_root(saved["COMFY_ROOT"], saved)
        self.assertEqual(setup.matching_folder_overrides(root, saved),
                         {"COMFY_INPUT_DIR": str(inputs), "COMFY_OUTPUT_DIR": str(outputs)})
        self.assertEqual(setup.matching_folder_overrides(self.data / "new-root", saved), {})

    def test_fresh_terminal_install_saves_split_comfy_folders(self):
        paths = {key: self.data / key.lower() for key in setup.OPTIONAL_SETTINGS}
        for folder in [*paths.values(), self.data / "comfy"]:
            folder.mkdir()
        overrides = {key: str(path) for key, path in paths.items()}
        with (
            patch.dict(os.environ, {**self.settings, **overrides}),
            patch("builtins.input", side_effect=["", "", "y"]),
            patch.object(setup, "probe_backend", return_value={}),
            patch.object(setup, "install_runtime") as install,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(setup.install(self.data / "settings.json"), 0)
        install.assert_called_once_with(self.data, {**self.settings, **overrides})

    def test_another_install_cannot_start_while_the_runtime_is_locked(self):
        with setup.install_lock(self.data), patch.object(setup, "stage_runtime") as stage:
            with self.assertRaises(setup.InstallFailure):
                setup.install_runtime(self.data, self.settings)
        stage.assert_not_called()
        self.assertEqual(setup.active_runtime(self.data), self.previous)
