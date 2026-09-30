#!/usr/bin/env python3
"""Fetch the matching installer from the newest earlier published release."""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
from pathlib import Path
import urllib.error
import urllib.request


def request_json(url: str, token: str) -> object:
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def download(url: str, token: str, destination: Path) -> None:
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/octet-stream",
            "Authorization": f"Bearer {token}",
        },
    )
    with urllib.request.urlopen(request, timeout=120) as response:
        destination.write_bytes(response.read())


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pattern", required=True, help="Installer asset glob")
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--current-tag", default=os.environ.get("GITHUB_REF_NAME"))
    parser.add_argument("--api-url", default=os.environ.get("GITHUB_API_URL", "https://api.github.com"))
    args = parser.parse_args()

    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if not args.repository or not args.current_tag or not token:
        raise SystemExit("repository, current tag, and a read-only GitHub token are required")
    releases = request_json(
        f"{args.api_url.rstrip('/')}/repos/{args.repository}/releases?per_page=100",
        token,
    )
    if not isinstance(releases, list):
        raise SystemExit("GitHub release API returned an unexpected response")
    previous = next(
        (
            release
            for release in releases
            if isinstance(release, dict)
            and release.get("tag_name") != args.current_tag
            and not release.get("draft")
            and not release.get("prerelease")
        ),
        None,
    )
    if previous is None:
        return 0
    assets = previous.get("assets")
    if not isinstance(assets, list):
        return 0
    artifact = next(
        (
            asset
            for asset in assets
            if isinstance(asset, dict)
            and isinstance(asset.get("name"), str)
            and fnmatch.fnmatchcase(asset["name"], args.pattern)
        ),
        None,
    )
    if artifact is None or not isinstance(artifact.get("url"), str):
        return 0

    args.output_dir.mkdir(parents=True, exist_ok=True)
    output = args.output_dir / artifact["name"]
    download(artifact["url"], token, output)
    by_name = {
        asset.get("name"): asset
        for asset in assets
        if isinstance(asset, dict) and isinstance(asset.get("name"), str)
    }
    for suffix in (".sigstore.json", ".provenance.json"):
        sidecar = by_name.get(f"{artifact['name']}{suffix}")
        if sidecar and isinstance(sidecar.get("url"), str):
            download(sidecar["url"], token, Path(f"{output}{suffix}"))
    print(output)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except urllib.error.URLError as error:
        raise SystemExit(f"could not fetch previous release artifact: {error}") from error
