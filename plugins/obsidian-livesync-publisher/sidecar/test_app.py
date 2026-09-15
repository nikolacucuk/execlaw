import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import app


class PublisherSourceTests(unittest.TestCase):
    def test_source_files_uses_configured_relative_subdirectory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "notes").mkdir()
            (root / "notes" / "one.md").write_text("# one\n", encoding="utf-8")
            with patch.object(app, "VAULT_ROOT", root):
                files = app.source_files("notes", 10, 1024)
            self.assertEqual(files[0][0], "notes/one.md")

    def test_source_files_rejects_path_escape(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(app, "VAULT_ROOT", Path(directory)):
                with self.assertRaises(ValueError):
                    app.source_files("../outside", 10, 1024)

    def test_source_status_reports_markdown_count(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "execlaw").mkdir()
            (root / "execlaw" / "one.md").write_text("one", encoding="utf-8")
            with patch.object(app, "VAULT_ROOT", root):
                status = app.source_status("execlaw")
            self.assertEqual(status["markdown_files"], 1)
            self.assertEqual(status["source"], str(root / "execlaw"))


if __name__ == "__main__":
    unittest.main()
