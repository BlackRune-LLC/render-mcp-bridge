import io
from pathlib import Path
import runpy
import stat
import tempfile
import unittest
import zipfile


NODE_ARCHIVE = Path(__file__).parent / "installer" / "src" / "node_archive.py"
review_archive = runpy.run_path(str(NODE_ARCHIVE))["review_archive"]


class NodeArchiveTests(unittest.TestCase):
    folder = "pack-0123456789abcdef0123456789abcdef01234567"

    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.output = Path(self.work.name)
        self.archive = self.output / "test.zip"
        self.stage = self.output / "staged"
        self.stage.mkdir()

    def review(self, *members, root=None):
        folder = root if root is not None else self.folder
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.comment = self.folder.rsplit("-", 1)[-1].encode()
            archive.writestr(f"{folder}/__init__.py", "raise RuntimeError('must not run')")
            for entry, content in members:
                name = f"{folder}/{entry}" if isinstance(entry, str) else entry
                archive.writestr(name, content)
        self.archive.write_bytes(payload.getvalue())
        return review_archive(self.archive, self.stage, self.folder)

    def test_reports_dependencies_and_hooks_without_loading_source(self):
        details = self.review(
            ("requirements.txt", "test-package"),
            ("prestartup_script.py", "raise RuntimeError('must not run')"),
        )
        self.assertEqual(details["files"], 3)
        self.assertEqual(details["dependencies"], ["requirements.txt"])
        self.assertEqual(details["startup_hooks"], ["prestartup_script.py"])
        self.assertTrue((self.stage / "__init__.py").is_file())

    def test_rejects_symbolic_link(self):
        link = zipfile.ZipInfo(f"{self.folder}/link.py")
        link.create_system = 3
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        with self.assertRaises(ValueError):
            self.review((link, "C:/Windows"))

    def test_rejects_escape_and_does_not_write_outside_stage(self):
        with self.assertRaises(ValueError):
            self.review(("../escaped.txt", "bad"))
        self.assertFalse((self.output / "escaped.txt").exists())

    def test_rejects_case_collision(self):
        with self.assertRaises(ValueError):
            self.review(("Case/one.txt", "a"), ("case/two.txt", "b"))

    def test_rejects_windows_reserved_path(self):
        with self.assertRaises(ValueError):
            self.review(("folder/NUL.py", "bad"))

    def test_rejects_wrong_commit_folder(self):
        with self.assertRaises(ValueError):
            self.review(root="other-revision")


if __name__ == "__main__":
    unittest.main()
