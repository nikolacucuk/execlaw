from __future__ import annotations

from collections import deque
from dataclasses import dataclass
from ipaddress import ip_address
from typing import Any, Dict, List, Literal, Optional, Set
from urllib.parse import urljoin, urlparse

import socket

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

from scrapling.fetchers import DynamicFetcher, Fetcher, StealthyFetcher

app = FastAPI(title="execlaw web-scraper sidecar", version="0.1.0")


class ApiError(Exception):
    def __init__(self, code: str, message: str, retryable: bool = False, status_code: int = 400):
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable
        self.status_code = status_code


@app.exception_handler(ApiError)
async def api_error_handler(_, exc: ApiError):
    raise HTTPException(
        status_code=exc.status_code,
        detail={"error": {"code": exc.code, "message": exc.message, "retryable": exc.retryable}},
    )


Mode = Literal["static", "dynamic", "stealthy"]


class FieldRule(BaseModel):
    name: str
    selector: str
    selector_type: Literal["css", "xpath"] = "css"
    extract: Literal["text", "html", "attr"] = "text"
    attr: Optional[str] = None
    all: bool = False


class FetchRequest(BaseModel):
    url: str
    mode: Mode = "static"
    session_id: Optional[str] = None
    wait_for: Optional[str] = None
    timeout_ms: int = Field(default=15000, ge=1000, le=120000)
    max_chars: int = Field(default=6000, ge=512, le=50000)
    allowed_domains: Optional[List[str]] = None


class ExtractRequest(BaseModel):
    url: str
    mode: Mode = "dynamic"
    session_id: Optional[str] = None
    fields: List[FieldRule] = Field(default_factory=list)
    main_text: bool = True
    include_links: bool = False
    timeout_ms: int = Field(default=30000, ge=1000, le=180000)
    max_chars: int = Field(default=12000, ge=512, le=100000)
    allowed_domains: Optional[List[str]] = None


class CrawlExtract(BaseModel):
    main_text: bool = True
    fields: List[FieldRule] = Field(default_factory=list)


class CrawlRequest(BaseModel):
    seed_url: str
    mode: Mode = "static"
    max_pages: int = Field(default=5, ge=1, le=25)
    max_depth: int = Field(default=1, ge=0, le=3)
    timeout_ms: int = Field(default=60000, ge=1000, le=300000)
    include_patterns: Optional[List[str]] = None
    exclude_patterns: Optional[List[str]] = None
    extract: Optional[CrawlExtract] = None
    allowed_domains: Optional[List[str]] = None


class SessionCloseRequest(BaseModel):
    session_id: str


@dataclass
class PageData:
    final_url: str
    status: int
    content_type: str
    title: Optional[str]
    text: str
    html_excerpt: str
    links: List[str]


def _normalize_domains(domains: Optional[List[str]]) -> Set[str]:
    if not domains:
        return set()
    return {d.strip().lower() for d in domains if d and d.strip()}


def _hostname(url: str) -> str:
    p = urlparse(url)
    if p.scheme not in {"http", "https"}:
        raise ApiError("invalid_scheme", "only http and https are allowed")
    if not p.hostname:
        raise ApiError("invalid_url", "URL has no hostname")
    return p.hostname.lower()


def _is_private_host(host: str) -> bool:
    if host == "localhost" or host.endswith(".localhost"):
        return True
    try:
        ip = ip_address(host)
        return (
            ip.is_private
            or ip.is_loopback
            or ip.is_link_local
            or ip.is_multicast
            or ip.is_unspecified
            or ip.is_reserved
        )
    except ValueError:
        pass

    try:
        for family, _, _, _, sockaddr in socket.getaddrinfo(host, None):
            ip_raw = sockaddr[0]
            ip = ip_address(ip_raw)
            if (
                ip.is_private
                or ip.is_loopback
                or ip.is_link_local
                or ip.is_multicast
                or ip.is_unspecified
                or ip.is_reserved
            ):
                return True
    except Exception:
        # DNS failures are treated as non-private here and handled during fetch.
        return False
    return False


def _enforce_host_policy(url: str, allowed_domains: Optional[List[str]]) -> None:
    host = _hostname(url)
    if _is_private_host(host):
        raise ApiError("blocked_host", "private or loopback hosts are not allowed")

    allowed = _normalize_domains(allowed_domains)
    if allowed:
        if host not in allowed and not any(host.endswith(f".{d}") for d in allowed):
            raise ApiError("domain_not_allowed", f"host '{host}' is not in allowed_domains")


def _clip(text: str, max_chars: int) -> tuple[str, bool]:
    if len(text) <= max_chars:
        return text, False
    return text[: max_chars - 1] + "...", True


def _to_text(result: Any) -> str:
    if result is None:
        return ""
    if isinstance(result, str):
        return result
    return str(result)


def _fetch(url: str, mode: Mode, timeout_ms: int) -> Any:
    # This skeleton intentionally uses the one-shot APIs.
    if mode == "dynamic":
        return DynamicFetcher.fetch(url, headless=True, network_idle=True)
    if mode == "stealthy":
        return StealthyFetcher.fetch(url, headless=True, network_idle=True)
    return Fetcher.get(url)


def _extract_links(page: Any, base_url: str) -> List[str]:
    try:
        hrefs = page.css("a::attr(href)").getall()
    except Exception:
        return []
    out: List[str] = []
    seen: Set[str] = set()
    for href in hrefs:
        if not href:
            continue
        abs_url = urljoin(base_url, href)
        p = urlparse(abs_url)
        if p.scheme not in {"http", "https"}:
            continue
        if abs_url not in seen:
            seen.add(abs_url)
            out.append(abs_url)
    return out


def _extract_fields(page: Any, fields: List[FieldRule]) -> Dict[str, Any]:
    data: Dict[str, Any] = {}
    for rule in fields:
        try:
            nodes = page.css(rule.selector) if rule.selector_type == "css" else page.xpath(rule.selector)
        except Exception:
            data[rule.name] = None
            continue

        values: List[Any] = []
        if rule.extract == "html":
            try:
                values = [n.html for n in nodes]
            except Exception:
                values = []
        elif rule.extract == "attr":
            if not rule.attr:
                values = []
            else:
                try:
                    values = [n.attrib.get(rule.attr) for n in nodes if getattr(n, "attrib", None)]
                except Exception:
                    values = []
        else:
            try:
                values = [n.get() if hasattr(n, "get") else _to_text(n) for n in nodes]
            except Exception:
                values = []

        if rule.all:
            data[rule.name] = values
        else:
            data[rule.name] = values[0] if values else None
    return data


def _page_data(url: str, mode: Mode, timeout_ms: int, max_chars: int) -> PageData:
    page = _fetch(url, mode, timeout_ms)

    final_url = _to_text(getattr(page, "url", url))
    status = int(getattr(page, "status", 200) or 200)
    content_type = _to_text(getattr(page, "content_type", "text/html"))
    title = None
    try:
        title = page.css("title::text").get()
    except Exception:
        title = None

    text_raw = _to_text(getattr(page, "text", ""))
    if not text_raw:
        try:
            text_raw = _to_text(page.get())
        except Exception:
            text_raw = ""
    text, _ = _clip(text_raw, max_chars)

    html_raw = _to_text(getattr(page, "html", ""))
    html_excerpt, _ = _clip(html_raw, min(max_chars, 8000))

    links = _extract_links(page, final_url)

    return PageData(
        final_url=final_url,
        status=status,
        content_type=content_type,
        title=title,
        text=text,
        html_excerpt=html_excerpt,
        links=links,
    )


@app.get("/healthz")
def healthz() -> Dict[str, Any]:
    return {"ok": True, "version": "0.1.0"}


@app.post("/v1/fetch")
def fetch(req: FetchRequest) -> Dict[str, Any]:
    _enforce_host_policy(req.url, req.allowed_domains)
    page = _page_data(req.url, req.mode, req.timeout_ms, req.max_chars)

    text, truncated = _clip(page.text, req.max_chars)
    return {
        "final_url": page.final_url,
        "status": page.status,
        "content_type": page.content_type,
        "title": page.title,
        "text": text,
        "html_excerpt": page.html_excerpt,
        "truncated": truncated,
        "timings_ms": {"fetch": 0, "render": 0, "extract": 0},
    }


@app.post("/v1/extract")
def extract(req: ExtractRequest) -> Dict[str, Any]:
    _enforce_host_policy(req.url, req.allowed_domains)
    page = _fetch(req.url, req.mode, req.timeout_ms)

    final_url = _to_text(getattr(page, "url", req.url))
    status = int(getattr(page, "status", 200) or 200)
    content_type = _to_text(getattr(page, "content_type", "text/html"))

    fields = _extract_fields(page, req.fields)

    main_text = ""
    if req.main_text:
        raw = _to_text(getattr(page, "text", ""))
        main_text, _ = _clip(raw, req.max_chars)

    links: List[str] = []
    if req.include_links:
        links = _extract_links(page, final_url)

    return {
        "final_url": final_url,
        "status": status,
        "content_type": content_type,
        "fields": fields,
        "main_text": main_text,
        "links": links,
        "truncated": len(main_text) >= req.max_chars if req.main_text else False,
        "timings_ms": {"fetch": 0, "render": 0, "extract": 0},
    }


@app.post("/v1/crawl")
def crawl(req: CrawlRequest) -> Dict[str, Any]:
    _enforce_host_policy(req.seed_url, req.allowed_domains)

    queue: deque[tuple[str, int]] = deque([(req.seed_url, 0)])
    visited: Set[str] = set()
    pages: List[Dict[str, Any]] = []

    while queue and len(visited) < req.max_pages:
        url, depth = queue.popleft()
        if url in visited:
            continue
        visited.add(url)

        try:
            _enforce_host_policy(url, req.allowed_domains)
            page = _fetch(url, req.mode, req.timeout_ms)
            final_url = _to_text(getattr(page, "url", url))
            status = int(getattr(page, "status", 200) or 200)

            item: Dict[str, Any] = {
                "url": final_url,
                "status": status,
                "title": _to_text(page.css("title::text").get()) if hasattr(page, "css") else None,
            }

            if req.extract and req.extract.main_text:
                item["main_text"] = _to_text(getattr(page, "text", ""))[:4000]
            if req.extract and req.extract.fields:
                item["fields"] = _extract_fields(page, req.extract.fields)

            pages.append(item)

            if depth < req.max_depth:
                for link in _extract_links(page, final_url):
                    if link not in visited:
                        queue.append((link, depth + 1))
        except Exception as exc:
            pages.append({"url": url, "status": 0, "error": str(exc)})

    return {
        "seed_url": req.seed_url,
        "visited": len(visited),
        "timed_out": False,
        "pages": pages,
        "limits": {
            "max_pages": req.max_pages,
            "max_depth": req.max_depth,
            "timeout_ms": req.timeout_ms,
        },
    }


@app.post("/v1/session/close")
def session_close(req: SessionCloseRequest) -> Dict[str, Any]:
    # One-shot fetchers in this skeleton do not persist browser/session state.
    # Keeping endpoint for API compatibility with the plugin contract.
    return {"ok": True, "session_id": req.session_id}
