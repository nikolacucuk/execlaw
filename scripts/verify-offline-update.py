#!/usr/bin/env python3
"""Verify an offline update bundle's contents and Sigstore attestations."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tempfile
import zipfile

ISSUER = "https://token.actions.githubusercontent.com"


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def valid_archive_path(name: str) -> bool:
    path = PurePosixPath(name)
    return not path.is_absolute() and all(part not in ("", ".", "..") for part in path.parts)


def verify_attestation(
    artifact: Path,
    bundle: Path,
    repository: str,
    version_pattern: str,
) -> None:
    identity = (
        rf"^https://github\.com/{re.escape(repository)}/\.github/workflows/"
        rf"(linux|macos|windows)-bundle\.yml@refs/tags/{version_pattern}$"
    )
    subprocess.run(
        [
            "cosign",
            "verify-blob-attestation",
            "--type",
            "slsaprovenance",
            "--bundle",
            str(bundle),
            "--certificate-oidc-issuer",
            ISSUER,
            "--certificate-identity-regexp",
            identity,
            str(artifact),
        ],
        check=True,
        stdout=subprocess.DEVNULL,
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bundle", required=True, type=Path)
    parser.add_argument("--repository", required=True, help="GitHub owner/repository")
    args = parser.parse_args()

    archive = args.bundle.resolve()
    outer_signature = Path(f"{archive}.sigstore.json")
    if not archive.is_file() or not outer_signature.is_file():
        raise SystemExit("offline update archive or its Sigstore bundle is missing")
    with tempfile.TemporaryDirectory(prefix="execlaw-offline-verify-") as temp_dir:
        outer_copy = Path(temp_dir) / archive.name
        outer_copy.write_bytes(archive.read_bytes())
        verify_attestation(
            outer_copy,
            outer_signature,
            args.repository,
            r"[^/]+",
        )

        with zipfile.ZipFile(archive) as bundle:
            names = bundle.namelist()
            if len(names) != len(set(names)) or any(not valid_archive_path(name) for name in names):
                raise SystemExit("offline update archive contains duplicate or unsafe paths")
            try:
                manifest = json.loads(bundle.read("manifest.json"))
            except (KeyError, json.JSONDecodeError) as error:
                raise SystemExit(f"offline update manifest is invalid: {error}") from error
            if manifest.get("format") != "execlaw-offline-update-v1":
                raise SystemExit("unsupported offline update manifest format")
            artifacts = manifest.get("artifacts")
            if not isinstance(artifacts, list):
                raise SystemExit("offline update manifest has no artifact inventory")
            roles = {entry.get("role") for entry in artifacts if isinstance(entry, dict)}
            if "current" not in roles:
                raise SystemExit("offline update bundle has no current installer")
            for entry in artifacts:
                if not isinstance(entry, dict):
                    raise SystemExit("offline update artifact entry is malformed")
                path = entry.get("archive_path")
                if not isinstance(path, str) or not valid_archive_path(path):
                    raise SystemExit("offline update manifest contains an unsafe artifact path")
                data = bundle.read(path)
                if len(data) != entry.get("size_bytes") or sha256_bytes(data) != entry.get("sha256"):
                    raise SystemExit(f"offline update artifact digest mismatch: {path}")
                if path.endswith(".sigstore.json"):
                    continue
                if path.endswith(".provenance.json"):
                    continue
                if path.endswith(".spdx.json"):
                    continue
                if entry.get("role") == "current":
                    sidecar_prefix = f"artifacts/current/{Path(path).name}"
                    if f"{sidecar_prefix}.sigstore.json" not in names or f"{sidecar_prefix}.provenance.json" not in names:
                        raise SystemExit("current installer is missing its offline signature or provenance")
                    artifact_copy = Path(temp_dir) / Path(path).name
                    signature_copy = Path(temp_dir) / f"{Path(path).name}.sigstore.json"
                    artifact_copy.write_bytes(data)
                    signature_copy.write_bytes(bundle.read(f"{sidecar_prefix}.sigstore.json"))
                    verify_attestation(
                        artifact_copy,
                        signature_copy,
                        args.repository,
                        r"[^/]+",
                    )
    print(f"verified offline update bundle: {archive.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
