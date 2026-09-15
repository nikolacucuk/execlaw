"""Small, bounded CouchDB publisher for an Obsidian LiveSync vault.

The sidecar reads only /vault/execlaw, never CouchDB's data directory. It
writes ordinary, unencrypted LiveSync plain-file metadata and leaf chunks.
Obsidian LiveSync remains responsible for downloading and resolving conflicts.
"""
from __future__ import annotations

import base64
import hashlib
import json
import mimetypes
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from time import time_ns
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit
from urllib.request import Request, urlopen

ROOT = Path("/vault/execlaw")
PORT = int(os.environ.get("PORT", "8080"))
MAX_REQUEST_BYTES = 256 * 1024


def json_response(handler: BaseHTTPRequestHandler, status: int, body: object) -> None:
    encoded = json.dumps(body, separators=(",", ":")).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(encoded)))
    handler.end_headers()
    handler.wfile.write(encoded)


def request_json(url: str, username: str, password: str, method: str = "GET", body: object | None = None) -> tuple[int, dict]:
    parsed = urlsplit(url)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise ValueError("couchdb_url must be an absolute http(s) URL")
    raw_auth = base64.b64encode(f"{username}:{password}".encode()).decode("ascii")
    payload = None if body is None else json.dumps(body, separators=(",", ":")).encode("utf-8")
    request = Request(
        url,
        data=payload,
        method=method,
        headers={
            "Accept": "application/json",
            "Authorization": f"Basic {raw_auth}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urlopen(request, timeout=30) as response:
            content = response.read()
            return response.status, json.loads(content or b"{}")
    except HTTPError as error:
        content = error.read()
        try:
            decoded = json.loads(content or b"{}")
        except json.JSONDecodeError:
            decoded = {"error": error.reason}
        return error.code, decoded
    except URLError as error:
        raise RuntimeError(f"CouchDB request failed: {error.reason}") from error


def couch_url(base: str, database: str, document_id: str | None = None) -> str:
    root = base.rstrip("/") + "/" + quote(database, safe="")
    return root if document_id is None else root + "/" + quote(document_id, safe="")


def document_id(path: str) -> str:
    # LiveSync's default non-obfuscated ordinary-file IDs use the f: namespace.
    return "f:" + path


def chunk_id(data: str) -> str:
    return "h:" + hashlib.sha256(data.encode("utf-8")).hexdigest()


def existing_document(base: str, database: str, doc_id: str, username: str, password: str) -> dict | None:
    status, body = request_json(couch_url(base, database, doc_id), username, password)
    if status == 404:
        return None
    if status >= 300:
        raise RuntimeError(f"CouchDB read failed for {doc_id}: HTTP {status}")
    return body


def put_document(base: str, database: str, document: dict, username: str, password: str) -> bool:
    status, body = request_json(
        couch_url(base, database, str(document["_id"])),
        username,
        password,
        method="PUT",
        body=document,
    )
    if status == 409:
        raise RuntimeError(f"CouchDB conflict for {document['_id']}; run again after LiveSync settles")
    if status >= 300:
        raise RuntimeError(f"CouchDB write failed for {document['_id']}: HTTP {status} {body}")
    return True


def source_files(max_files: int, max_bytes: int) -> list[tuple[str, Path, int]]:
    if not ROOT.is_dir():
        raise RuntimeError(f"publisher source directory is missing: {ROOT}")
    files: list[tuple[str, Path, int]] = []
    total = 0
    for path in sorted(ROOT.rglob("*.md")):
        if not path.is_file() or any(part.startswith(".") for part in path.relative_to(ROOT).parts):
            continue
        size = path.stat().st_size
        total += size
        if len(files) >= max_files:
            raise RuntimeError(f"publisher file limit exceeded ({max_files})")
        if total > max_bytes:
            raise RuntimeError(f"publisher byte limit exceeded ({max_bytes})")
        relative = path.relative_to(ROOT).as_posix()
        files.append(("execlaw/" + relative, path, size))
    return files


def publish(payload: dict) -> dict:
    base = str(payload.get("couchdb_url", "")).strip()
    database = str(payload.get("database", "")).strip()
    username = str(payload.get("username", ""))
    password = str(payload.get("password", ""))
    max_files = max(1, min(int(payload.get("max_files", 1000)), 10000))
    max_bytes = max(1, min(int(payload.get("max_bytes", 52428800)), 52428800))
    if not base or not database or not username or not password:
        raise ValueError("couchdb_url, database, username, and password are required")

    files = source_files(max_files, max_bytes)
    published = 0
    skipped = 0
    chunks = 0
    for logical_path, file_path, size in files:
        content = file_path.read_text(encoding="utf-8")
        child = chunk_id(content)
        chunk = {"_id": child, "type": "leaf", "data": content}
        if existing_document(base, database, child, username, password) is None:
            put_document(base, database, chunk, username, password)
            chunks += 1

        stat = file_path.stat()
        metadata_id = document_id(logical_path)
        old = existing_document(base, database, metadata_id, username, password)
        metadata = {
            "_id": metadata_id,
            "path": logical_path,
            "ctime": int(stat.st_ctime_ns // 1_000_000),
            "mtime": int(stat.st_mtime_ns // 1_000_000),
            "size": size,
            "type": "plain",
            "children": [child],
            "eden": {},
        }
        if old and old.get("children") == [child] and old.get("size") == size:
            skipped += 1
            continue
        if old and old.get("_rev"):
            metadata["_rev"] = old["_rev"]
        put_document(base, database, metadata, username, password)
        published += 1

    return {"ok": True, "files_seen": len(files), "metadata_written": published, "files_unchanged": skipped, "chunks_written": chunks, "deletions": 0}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        if self.path == "/healthz":
            json_response(self, 200, {"ok": True, "source": str(ROOT)})
            return
        json_response(self, 404, {"error": "not found"})

    def do_POST(self) -> None:
        if self.path != "/v1/publish":
            json_response(self, 404, {"error": "not found"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length <= 0 or length > MAX_REQUEST_BYTES:
                raise ValueError("request body is missing or too large")
            payload = json.loads(self.rfile.read(length))
            result = publish(payload)
            json_response(self, 200, result)
        except (ValueError, RuntimeError, OSError, UnicodeError) as error:
            json_response(self, 400, {"ok": False, "error": str(error)})
        except Exception:
            json_response(self, 500, {"ok": False, "error": "publisher failed"})

    def log_message(self, format: str, *args: object) -> None:
        # Credentials are never included in request URLs or log messages.
        return


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
