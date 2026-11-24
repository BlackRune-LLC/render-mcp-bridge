import contextlib
import io
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
