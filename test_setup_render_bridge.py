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

    def test_shared_gaming_flag_is_loaded_and_used_for_start_and_controls(self):
        shared_flag = self.data / "shared gaming state" / "gaming_mode.flag"
        setup.save_settings(self.previous / "settings.json", {**self.settings, "PAUSE_FLAG": str(shared_flag)})
        python = setup.venv_python(self.previous)
        python.parent.mkdir(parents=True)
        python.touch()
        for kind in ("input", "output"):
            (self.data / "comfy" / kind).mkdir(parents=True)
        for args in ([], ["gaming-mode", "status"], ["gaming-mode", "off"]):
            for inherited in ({}, {"PAUSE_FLAG": "legacy-override.flag"}):
                with (
                    self.subTest(args=args, inherited=inherited),
                    patch.dict(os.environ, inherited, clear=True),
                    patch.object(setup.subprocess, "run", return_value=Mock(returncode=0)) as run,
                ):
                    self.assertEqual(setup.launch(self.data / "settings.json", args), 0)
                    self.assertEqual(run.call_args.kwargs["env"]["PAUSE_FLAG"],
                                     inherited.get("PAUSE_FLAG", str(shared_flag)))
        self.assertFalse(shared_flag.parent.exists())

    def test_shared_gaming_flag_settings_reject_unsafe_paths(self):
        reusable = self.data / "reusable.json"
        for flag in (
            "gaming_mode.flag",
            os.path.join("relative", "gaming_mode.flag"),
            str(self.data / "another.flag"),
            str(self.data / "GAMING_MODE.FLAG"),
            str(self.data / "gaming_mode.flag") + " ",
            str(self.data / "gaming_mode.flag") + os.sep,
            os.path.join(str(self.data), ".", "gaming_mode.flag"),
            os.path.join(str(self.data), "folder", "..", "gaming_mode.flag"),
            os.path.join(str(self.data), "bad\x00path", "gaming_mode.flag"),
        ):
            with self.subTest(flag=flag):
                setup.save_settings(reusable, {**self.settings, "PAUSE_FLAG": flag})
                with self.assertRaisesRegex(ValueError, "gaming_mode.flag"):
                    setup.load_settings(reusable)
                with self.assertRaisesRegex(ValueError, "gaming_mode.flag"):
                    setup.portable_settings(reusable)

    def test_upgrade_keeps_the_saved_gaming_flag_and_does_not_store_the_env_override(self):
        for kind in ("input", "output"):
            (self.data / "comfy" / kind).mkdir(parents=True)
        saved = {**self.settings, "PAUSE_FLAG": str(self.data / "saved" / "gaming_mode.flag")}
        existing = self.previous / "settings.json"
        setup.save_settings(existing, saved)
        reusable = self.data / "reusable.json"
        setup.save_settings(reusable, {"PAUSE_FLAG": str(self.data / "other" / "gaming_mode.flag")})
        with (
            patch.dict(os.environ, {"PAUSE_FLAG": "legacy-override.flag"}, clear=True),
            patch("builtins.input", return_value="y"),
            patch.object(setup, "probe_backend", return_value={}),
            patch.object(setup, "install_runtime") as install,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(setup.install(self.data / "settings.json", reusable), 0)
        self.assertEqual(install.call_args.args, (self.data, saved))
        self.assertEqual(setup.load_settings(existing), saved)

    def test_install_smoke_cannot_access_the_shared_gaming_flag(self):
        shared_flag = self.data / "shared" / "gaming_mode.flag"
        shared_flag.parent.mkdir()
        shared_flag.write_text("fixture paused", encoding="utf-8")
        preferences = {**self.settings, "PAUSE_FLAG": str(shared_flag)}
        with (
            patch.dict(os.environ, {"PAUSE_FLAG": str(shared_flag)}),
            patch.object(setup, "run_install_stage") as stages,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            setup.install_runtime(self.data, preferences)
        smoke = stages.call_args.args[4]
        self.assertEqual(Path(smoke["PAUSE_FLAG"]),
                         Path(smoke["COMFY_MCP_DATA_DIR"]) / "gaming_mode.flag")
        self.assertFalse(Path(smoke["PAUSE_FLAG"]).exists())
        self.assertEqual(shared_flag.read_text(encoding="utf-8"), "fixture paused")
        self.assertEqual(setup.load_settings(setup.active_runtime(self.data) / "settings.json"), preferences)

    def test_attach_only_installation_blocks_start_but_keeps_gaming_mode_controls(self):
        (self.data / "attach-only").touch()
        python = setup.venv_python(self.previous)
        python.parent.mkdir(parents=True)
        python.touch()
        for kind in ("input", "output"):
            (self.data / "comfy" / kind).mkdir(parents=True)
        with (
            patch.object(setup.subprocess, "run", return_value=Mock(returncode=0)) as run,
            contextlib.redirect_stderr(io.StringIO()) as errors,
        ):
            self.assertEqual(setup.launch(self.data / "settings.json", []), 1)
            run.assert_not_called()
            self.assertEqual(setup.launch(self.data / "settings.json", ["gaming-mode", "status"]), 0)
        self.assertIn("external server", errors.getvalue())
        self.assertEqual(run.call_args.args[0][-2:], ["gaming-mode", "status"])

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

    def test_setup_accepts_input_and_output_folders_outside_the_comfy_root(self):
        inputs = self.data / "custom-input"
        outputs = self.data / "custom-output"
        root = Path(self.settings["COMFY_ROOT"])
        for folder in (root, inputs, outputs):
            folder.mkdir()
        overrides = {"COMFY_INPUT_DIR": str(inputs), "COMFY_OUTPUT_DIR": str(outputs)}
        self.assertEqual(setup.comfy_root(str(root), overrides), root)

    def test_fresh_terminal_install_saves_split_comfy_folders(self):
        (self.data / setup.ACTIVE_RUNTIME).unlink()
        paths = {key: self.data / key.lower() for key in setup.OPTIONAL_FOLDERS}
        for folder in [*paths.values(), self.data / "comfy"]:
            folder.mkdir()
        overrides = {key: str(path) for key, path in paths.items()}
        with (
            patch.dict(os.environ, {**self.settings, **overrides}, clear=True),
            patch("builtins.input", side_effect=["", "", "y"]),
            patch.object(setup, "probe_backend", return_value={}),
            patch.object(setup, "install_runtime") as install,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(setup.install(self.data / "settings.json"), 0)
        self.assertEqual(install.call_args.args, (self.data, {**self.settings, **overrides}))

    def test_terminal_config_skips_configured_prompts_and_previews_before_confirmation(self):
        (self.data / setup.ACTIVE_RUNTIME).unlink()
        root = Path(self.settings["COMFY_ROOT"])
        for kind in ("input", "output"):
            (root / kind).mkdir(parents=True)
        reusable = self.data / "reusable.json"
        flag = self.data / "shared" / "gaming_mode.flag"
        settings = {**self.settings, "PAUSE_FLAG": str(flag)}
        setup.save_settings(reusable, settings)
        with (
            patch.dict(os.environ, {"COMFY_ROOT": "unused", "COMFY_URL": "unused"}, clear=True),
            patch("builtins.input", return_value="y") as answer,
            patch.object(setup, "probe_backend", return_value={}),
            patch.object(setup, "install_runtime") as install,
            contextlib.redirect_stdout(io.StringIO()) as preview,
        ):
            self.assertEqual(setup.main(["--data-dir", str(self.data), "install", "--config", str(reusable)]), 0)
        answer.assert_called_once_with("Prepare and activate a verified Render Bridge runtime? [y/N] ")
        self.assertEqual(install.call_args.args, (self.data, settings))
        for value in (root, root / "input", root / "output", self.settings["COMFY_URL"], self.data, flag):
            self.assertIn(str(value), preview.getvalue())

    def test_upgrade_keeps_filled_settings_and_fills_blanks_from_the_config(self):
        root = Path(self.settings["COMFY_ROOT"])
        (root / "input").mkdir(parents=True)
        output = self.data / "custom-output"
        output.mkdir()
        existing = self.previous / "settings.json"
        setup.save_settings(existing, {**self.settings, "COMFY_OUTPUT_DIR": "  "})
        original = existing.read_bytes()
        reusable = self.data / "reusable.json"
        setup.save_settings(reusable, {
            "COMFY_ROOT": str(self.data / "different root"),
            "COMFY_URL": "http://localhost:9288",
            "COMFY_OUTPUT_DIR": str(output),
        })
        with (
            patch.dict(os.environ, {"COMFY_OUTPUT_DIR": "unused"}, clear=True),
            patch("builtins.input", return_value="y") as answer,
            patch.object(setup, "probe_backend", return_value={}),
            patch.object(setup, "install_runtime") as install,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(setup.install(self.data / "settings.json", reusable), 0)
        self.assertEqual(answer.call_count, 1)
        self.assertEqual(install.call_args.args, (self.data, {**self.settings, "COMFY_OUTPUT_DIR": str(output)}))
        self.assertEqual(existing.read_bytes(), original)

    def test_invalid_active_configuration_does_not_fall_back_to_the_supplied_file(self):
        reusable = self.data / "reusable.json"
        setup.save_settings(reusable, self.settings)
        existing = self.previous / "settings.json"
        existing.write_text('{"MCP_TOKEN": "fixture-secret"}', encoding="utf-8")
        original = existing.read_bytes()
        with (
            patch("builtins.input") as answer,
            patch.object(setup, "probe_backend") as probe,
            patch.object(setup, "install_runtime") as install,
            contextlib.redirect_stdout(io.StringIO()) as preview,
        ):
            self.assertEqual(setup.install(self.data / "settings.json", reusable), 1)
        answer.assert_not_called()
        probe.assert_not_called()
        install.assert_not_called()
        self.assertIn("saved settings", preview.getvalue())
        self.assertNotIn("fixture-secret", preview.getvalue())
        self.assertEqual(existing.read_bytes(), original)

    def test_terminal_rejects_invalid_reusable_settings_before_activation(self):
        (self.data / setup.ACTIVE_RUNTIME).unlink()
        reusable = self.data / "reusable.json"
        for value in (
            '{"MCP_TOKEN_FILE": "private"}',
            '{"COMFY_URL": true}',
            '{"COMFY_URL": "", "COMFY_URL": ""}',
            '{"COMFY_ROOT": "relative/folder"}',
            '{"COMFY_URL": "https://example.org:8188"}',
            ' ' * (64 * 1024 + 1),
        ):
            with self.subTest(value=value[:100]):
                reusable.write_text(value, encoding="utf-8")
                with (
                    patch("builtins.input") as answer,
                    patch.object(setup, "probe_backend") as probe,
                    patch.object(setup, "install_runtime") as install,
                    contextlib.redirect_stdout(io.StringIO()),
                ):
                    self.assertEqual(setup.install(self.data / "settings.json", reusable), 1)
                answer.assert_not_called()
                probe.assert_not_called()
                install.assert_not_called()

    def test_another_install_cannot_start_while_the_runtime_is_locked(self):
        with setup.install_lock(self.data), patch.object(setup, "stage_runtime") as stage:
            with self.assertRaises(setup.InstallFailure):
                setup.install_runtime(self.data, self.settings)
        stage.assert_not_called()
        self.assertEqual(setup.active_runtime(self.data), self.previous)

    def test_activation_preserves_settings_changed_after_the_preview(self):
        baseline = setup.installed_settings(self.data / "settings.json")
        updated = {**self.settings, "COMFY_URL": "http://localhost:9288"}
        setup.save_settings(self.previous / "settings.json", updated)
        with patch.object(setup, "stage_runtime") as stage:
            with self.assertRaisesRegex(setup.InstallFailure, "saved settings changed"):
                setup.install_runtime(self.data, self.settings, expected=baseline)
        stage.assert_not_called()
        self.assertEqual(setup.load_settings(self.previous / "settings.json"), updated)

    def test_activation_preserves_settings_changed_while_the_new_runtime_is_staging(self):
        baseline = setup.installed_settings(self.data / "settings.json")
        active_settings = self.previous / "settings.json"
        updated = {**self.settings, "COMFY_URL": "http://localhost:9288"}

        def update_during_startup_check(_command, _directory, phase, _timeout, _env=None):
            if phase == "Render Bridge startup check":
                setup.save_settings(active_settings, updated)

        with (
            patch.object(setup, "run_install_stage", side_effect=update_during_startup_check),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            with self.assertRaisesRegex(setup.InstallFailure, "saved settings changed"):
                setup.install_runtime(self.data, self.settings, expected=baseline)
        self.assertEqual(setup.active_runtime(self.data), self.previous)
        self.assertEqual(list(self.data.glob("app-*")), [self.previous])
        self.assertEqual(setup.load_settings(active_settings), updated)
        self.assertEqual((self.data / "token.txt").read_text(encoding="utf-8"), "fixture-token")
