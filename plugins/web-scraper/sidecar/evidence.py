"""Stable identifiers and bounded evidence metadata for local page captures."""

from __future__ import annotations

import hashlib
import time
from typing import Any, Dict
from urllib.parse import urlparse


def normalized_source_url(url: str) -> str:
    parsed = urlparse(url)
    return parsed._replace(
        scheme=parsed.scheme.lower(),
        netloc=parsed.netloc.lower(),
        fragment="",
    ).geturl()


def source_evidence(final_url: str, html: str, text: str, max_chars: int) -> Dict[str, Any]:
    """Bind a bounded text snapshot to the captured URL and rendered body hash."""
    canonical = normalized_source_url(final_url)
    truncated = len(text) > max_chars
    snapshot_text = text[: max_chars - 1] + "…" if truncated else text
    body = html or text
    return {
        "source_id": "src-" + hashlib.sha256(canonical.encode("utf-8")).hexdigest(),
        "retrieved_at": int(time.time()),
        "content_sha256": hashlib.sha256(body.encode("utf-8")).hexdigest(),
        "snapshot_text": snapshot_text,
        "snapshot_truncated": truncated,
    }
