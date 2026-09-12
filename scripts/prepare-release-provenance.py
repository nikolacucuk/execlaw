#!/usr/bin/env python3
"""Create offline Sigstore bundles and execlaw provenance sidecars for artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifacts", nargs="+", type=Path)
    parser.add_argument("--artifact-type", default="plugin_zip")
    args = parser.parse_args()

    repository_slug = os.environ["GITHUB_REPOSITORY"]
    source_repository = f"https://github.com/{repository_slug}"
    source_commit = os.environ["GITHUB_SHA"]
    workflow_ref = os.environ["GITHUB_WORKFLOW_REF"]
    workflow_identity = f"https://github.com/{workflow_ref}"
    oidc_issuer = "https://token.actions.githubusercontent.com"

    for artifact in args.artifacts:
        artifact = artifact.resolve()
        sbom = Path(f"{artifact}.spdx.json")
        if not sbom.is_file():
            raise SystemExit(f"missing SPDX SBOM for {artifact}: {sbom}")

        artifact_sha = sha256(artifact)
        sbom_sha = sha256(sbom)
        bundle = Path(f"{artifact}.sigstore.json")
        statement_path = Path(f"{artifact}.provenance.json")
        predicate = {
            "buildDefinition": {
                "buildType": "https://github.com/Attestations/GitHubActionsWorkflow@v1",
                "externalParameters": {"workflow": workflow_ref},
                "internalParameters": {"githubEventName": os.environ.get("GITHUB_EVENT_NAME", "")},
                "resolvedDependencies": [
                    {"uri": f"git+{source_repository}@{source_commit}", "digest": {"gitCommit": source_commit}}
                ],
            },
            "runDetails": {
                "builder": {"id": workflow_identity},
                "metadata": {"invocationId": os.environ.get("GITHUB_RUN_ID", "local")},
            },
        }
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as handle:
            json.dump(predicate, handle, separators=(",", ":"), sort_keys=True)
            predicate_path = Path(handle.name)
        try:
            subprocess.run(
                [
                    "cosign",
                    "attest-blob",
                    "--yes",
                    "--type",
                    "slsaprovenance",
                    "--predicate",
                    str(predicate_path),
                    "--bundle",
                    str(bundle),
                    str(artifact),
                ],
                check=True,
            )
        finally:
            predicate_path.unlink(missing_ok=True)

        statement = {
            "artifact_id": f"{args.artifact_type}:{artifact.name}:{artifact_sha}",
            "artifact_type": args.artifact_type,
            "artifact_locator": str(artifact),
            "sha256": artifact_sha,
            "publisher_identity": oidc_issuer,
            "source_repository": source_repository,
            "source_commit": source_commit,
            "workflow_identity": workflow_identity,
            "signature_reference": str(bundle),
            "attestation_result": "offline Sigstore SLSA provenance bundle",
            "sbom_format": "spdx",
            "sbom_location": str(sbom),
            "sbom_sha256": sbom_sha,
        }
        statement_path.write_text(json.dumps(statement, indent=2) + "\n", encoding="utf-8")
        print(f"prepared provenance for {artifact.name}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
