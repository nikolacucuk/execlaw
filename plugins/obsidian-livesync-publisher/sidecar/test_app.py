import tempfile
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from pathlib import Path
from unittest.mock import patch
from urllib.parse import unquote, urlsplit

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


class FakeCouchDBHandler(BaseHTTPRequestHandler):
    documents: dict[str, dict] = {}

    def do_GET(self) -> None:
        document_id = unquote(urlsplit(self.path).path.rsplit("/", 1)[-1])
        document = self.documents.get(document_id)
        if document is None:
            self.send_response(404)
            self.end_headers()
            return
        body = app.json.dumps(document).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_PUT(self) -> None:
        document_id = unquote(urlsplit(self.path).path.rsplit("/", 1)[-1])
        length = int(self.headers["Content-Length"])
        document = app.json.loads(self.rfile.read(length))
        document["_id"] = document_id
        document.setdefault("_rev", "1-test")
        self.documents[document_id] = document
        self.send_response(201)
        self.end_headers()

    def log_message(self, format: str, *args: object) -> None:
        return


class PublisherCouchDBTests(unittest.TestCase):
    def test_publish_writes_chunk_and_metadata_over_http(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "execlaw").mkdir()
            (root / "execlaw" / "one.md").write_text("# one\n", encoding="utf-8")
            FakeCouchDBHandler.documents = {}
            server = ThreadingHTTPServer(("127.0.0.1", 0), FakeCouchDBHandler)
            thread = Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                with patch.object(app, "VAULT_ROOT", root):
                    result = app.publish(
                        {
                            "couchdb_url": f"http://127.0.0.1:{server.server_port}",
                            "database": "djenka_db",
                            "username": "test-user",
                            "password": "test-password",
                            "source_subdir": "execlaw",
                            "max_files": 10,
                            "max_bytes": 1024,
                        }
                    )
            finally:
                server.shutdown()
                server.server_close()

        self.assertEqual(result["files_seen"], 1)
        self.assertEqual(result["metadata_written"], 1)
        self.assertEqual(result["chunks_written"], 1)
        self.assertIn("f:execlaw/one.md", FakeCouchDBHandler.documents)
        metadata = FakeCouchDBHandler.documents["f:execlaw/one.md"]
        self.assertEqual(metadata["path"], "execlaw/one.md")
        self.assertEqual(metadata["type"], "plain")
        self.assertEqual(len(metadata["children"]), 1)
        self.assertIn(metadata["children"][0], FakeCouchDBHandler.documents)


if __name__ == "__main__":
    unittest.main()
