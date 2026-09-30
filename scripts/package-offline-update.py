#!/usr/bin/env python3
"""Package signed desktop installers with an offline recovery contract."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import zipfile


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def clean_component(value: str, label: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,127}", value):
        raise SystemExit(f"invalid {label}: {value!r}")
    return value


def artifact_entries(path: Path, role: str) -> list[dict[str, str | int]]:
    path = path.resolve()
    if not path.is_file():
        raise SystemExit(f"release artifact is not a file: {path}")
    sidecars = [
        Path(f"{path}.sigstore.json"),
        Path(f"{path}.provenance.json"),
    ]
    missing = [sidecar for sidecar in sidecars if not sidecar.is_file()]
    if missing and role == "current":
        raise SystemExit(
            "offline release bundle requires artifact signature and provenance sidecars: "
            + ", ".join(str(item) for item in missing)
        )
    entries = []
    for source in [path, *(sidecar for sidecar in sidecars if sidecar.is_file())]:
        arcname = PurePosixPath("artifacts", role, source.name).as_posix()
        entries.append(
            {
                "archive_path": arcname,
                "sha256": sha256(source),
                "size_bytes": source.stat().st_size,
                "role": role,
            }
        )
    return entries


def recovery_instructions(platform: str, version: str, rollback_available: bool) -> str:
    rollback_line = (
        "The previous installer is bundled under artifacts/rollback/."
        if rollback_available
        else "This is an initial-install bundle and has no previous installer."
    )
    return f"""# execlaw offline update {version} ({platform})

Verify the outer bundle and every installer using the included verifier and
the repository owner identity before installation:

    python verify-offline-update.py --bundle <offline-update.zip> --repository <owner/repository>

Stop execlaw before making the backup. Use the same SQLCipher binary and
keyring as the installed service:

    execlaw backup --to <protected-pre-update-backup.db>

Install the package under artifacts/current/ using the platform package
manager, then run `execlaw doctor` from the installed location before starting
normal traffic. Keep the backup until the new service has completed startup
and its event chain has been checked.

If the update fails after a migration, restore the pre-update database first:

    execlaw restore --from <protected-pre-update-backup.db> \\
      --db <execlaw-database-path> --force

Then reinstall the previous package from artifacts/rollback/. Replacing only
the binary never downgrades a migrated schema. {rollback_line}

Database backups and their matching master key/event HMAC key are operator
secrets; this bundle contains installers and verification metadata only.
"""


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", required=True, type=Path)
    parser.add_argument("--rollback-artifact", type=Path)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    platform = clean_component(args.platform, "platform")
    version = clean_component(args.version, "version")
    output = args.output.resolve()
    if output.exists():
        raise SystemExit(f"refusing to overwrite offline bundle: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)

    entries = artifact_entries(args.artifact, "current")
    if args.rollback_artifact:
        entries.extend(artifact_entries(args.rollback_artifact, "rollback"))
    manifest = {
        "format": "execlaw-offline-update-v1",
        "platform": platform,
        "version": version,
        "artifacts": entries,
        "rollback_available": args.rollback_artifact is not None,
        "database_policy": {
            "backup_before_install": True,
            "restore_backup_before_reinstalling_previous_binary": True,
            "binary_replacement_downgrades_schema": False,
            "secrets_included": False,
        },
    }
    readme = recovery_instructions(
        platform, version, args.rollback_artifact is not None
    )
    with zipfile.ZipFile(output, "x", compression=zipfile.ZIP_DEFLATED) as bundle:
        bundle.writestr("manifest.json", json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        bundle.writestr("UPDATE.md", readme)
        verifier = Path(__file__).with_name("verify-offline-update.py")
        if not verifier.is_file():
            raise SystemExit(f"offline update verifier is missing: {verifier}")
        bundle.write(verifier, "verify-offline-update.py")
        for entry in entries:
            source_name = Path(str(entry["archive_path"])).name
            role = str(entry["role"])
            source = args.artifact if role == "current" else args.rollback_artifact
            assert source is not None
            resolved = source.resolve()
            candidate = resolved if source_name == resolved.name else Path(f"{resolved}.{source_name.removeprefix(resolved.name + '.')}")
            if not candidate.is_file() or sha256(candidate) != entry["sha256"]:
                raise SystemExit(f"artifact sidecar changed during packaging: {candidate}")
            bundle.write(candidate, str(entry["archive_path"]))
    print(f"packaged offline update bundle: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
