#!/usr/bin/env python3
"""Collect Criterion estimates and compare them on a pinned benchmark host."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import platform
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read JSON {path}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"expected JSON object in {path}")
    return value


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def cpu_model() -> str:
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.lower().startswith(("model name", "hardware")) and ":" in line:
                return line.split(":", 1)[1].strip()
    return platform.processor() or platform.machine()


def collect(args: argparse.Namespace) -> int:
    contract_path = Path(args.contract)
    contract = read_json(contract_path)
    criterion_root = Path(args.criterion_dir)
    benchmarks: dict[str, Any] = {}
    statistic = contract.get("statistic", "mean")
    expected_confidence = contract.get("confidence_level", 0.95)
    for entry in contract.get("benchmarks", []):
        name = entry["id"]
        criterion_path = entry.get("criterion_path", name)
        estimate_path = criterion_root.joinpath(*criterion_path.split("/"), "new", "estimates.json")
        estimates = read_json(estimate_path)
        estimate = estimates.get(statistic)
        interval = estimate.get("confidence_interval") if isinstance(estimate, dict) else None
        if not isinstance(interval, dict):
            raise ValueError(f"Criterion estimate missing {statistic} confidence interval: {name}")
        if not math.isclose(interval.get("confidence_level", 0.0), expected_confidence, abs_tol=1e-6):
            raise ValueError(f"Criterion confidence level does not match contract for {name}")
        point = estimate.get("point_estimate")
        low = interval.get("lower_bound")
        high = interval.get("upper_bound")
        if not all(isinstance(value, (int, float)) and math.isfinite(value) for value in (point, low, high)):
            raise ValueError(f"Criterion estimate contains invalid values: {name}")
        benchmarks[name] = {
            "statistic": statistic,
            "point_estimate_seconds": point,
            "confidence_low_seconds": low,
            "confidence_high_seconds": high,
            "regression_tolerance_percent": entry["regression_tolerance_percent"],
        }

    lock_path = Path("Cargo.lock")
    rustc = subprocess.run(
        ["rustc", "-Vv"], check=True, capture_output=True, text=True
    ).stdout.strip()
    rustc_digest = hashlib.sha256(rustc.encode()).hexdigest()
    metadata = {
        "runner_name": args.runner_name,
        "system": platform.system(),
        "machine": platform.machine(),
        "cpu_model": cpu_model(),
        "rustc_sha256": rustc_digest,
        "rustc_version": rustc.splitlines()[0] if rustc else "unknown",
        "cargo_lock_sha256": sha256_file(lock_path),
        "contract_sha256": sha256_file(contract_path),
        "commit": args.commit,
    }
    output = {
        "schema_version": 1,
        "collected_at": datetime.now(timezone.utc).isoformat(),
        "metadata": metadata,
        "benchmarks": benchmarks,
    }
    target = Path(args.output)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"collected {len(benchmarks)} benchmark estimates into {target}")
    return 0


def compare(args: argparse.Namespace) -> int:
    baseline_path = Path(args.baseline)
    current_path = Path(args.current)
    report_path = Path(args.report)
    current = read_json(current_path)
    report: dict[str, Any] = {
        "schema_version": 1,
        "baseline": str(baseline_path),
        "current": str(current_path),
        "current_metadata": current.get("metadata", {}),
        "results": [],
    }
    if args.replace_baseline:
        baseline_path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(current_path, baseline_path)
        report["status"] = "baseline_replaced_without_comparison"
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print("baseline replaced without comparison; review the stored run artifact")
        return 0
    if not baseline_path.is_file():
        if not args.bootstrap_if_missing:
            raise ValueError("stable-hardware baseline is missing; run the benchmark workflow on the default branch first")
        baseline_path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(current_path, baseline_path)
        report["status"] = "baseline_bootstrapped_without_comparison"
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print("baseline bootstrapped; this first run is recorded but not regression-gated")
        return 0

    baseline = read_json(baseline_path)
    base_meta = baseline.get("metadata", {})
    current_meta = current.get("metadata", {})
    identity_fields = ("runner_name", "system", "machine", "cpu_model", "rustc_sha256")
    mismatches = [key for key in identity_fields if base_meta.get(key) != current_meta.get(key)]
    if mismatches:
        raise ValueError(
            "baseline hardware/toolchain/contract identity differs for: " + ", ".join(mismatches)
        )

    baseline_benches = baseline.get("benchmarks", {})
    current_benches = current.get("benchmarks", {})
    regressions = 0
    compared = 0
    unbaselined: list[str] = []
    for name, current_estimate in current_benches.items():
        baseline_estimate = baseline_benches.get(name)
        if baseline_estimate is None:
            # New Criterion cases become baseline candidates on the next
            # successful default-branch run; existing matching cases still
            # gate this change so additive coverage doesn't deadlock merges.
            unbaselined.append(name)
            report["results"].append({"id": name, "status": "new_benchmark_pending_baseline"})
            continue
        compared += 1
        tolerance = current_estimate["regression_tolerance_percent"] / 100.0
        threshold = baseline_estimate["confidence_high_seconds"] * (1.0 + tolerance)
        regressed = current_estimate["confidence_low_seconds"] > threshold
        regressions += int(regressed)
        report["results"].append({
            "id": name,
            "baseline_seconds": baseline_estimate["point_estimate_seconds"],
            "current_seconds": current_estimate["point_estimate_seconds"],
            "baseline_confidence_high_seconds": baseline_estimate["confidence_high_seconds"],
            "current_confidence_low_seconds": current_estimate["confidence_low_seconds"],
            "regression_tolerance_percent": current_estimate["regression_tolerance_percent"],
            "status": "regression" if regressed else "within_noise_tolerance",
        })
    if compared == 0:
        if not args.bootstrap_if_missing:
            raise ValueError("no benchmark cases overlap the stored baseline")
        baseline_path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(current_path, baseline_path)
        report["status"] = "baseline_replaced_after_benchmark_contract_change"
        report["regression_count"] = 0
        report["compared_count"] = 0
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print("benchmark contract has no overlapping cases; baseline refreshed on default branch")
        return 0
    report["status"] = (
        "failed"
        if regressions
        else "passed_with_unbaselined_benchmarks"
        if unbaselined
        else "passed"
    )
    report["regression_count"] = regressions
    report["compared_count"] = compared
    report["unbaselined_benchmarks"] = unbaselined
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    for result in report["results"]:
        if result["status"] == "new_benchmark_pending_baseline":
            print(f"unbaselined: {result['id']} (will baseline on the default branch)")
            continue
        print(
            f"{result['status']}: {result['id']} "
            f"baseline={result['baseline_seconds']:.9g}s "
            f"current={result['current_seconds']:.9g}s "
            f"tolerance={result['regression_tolerance_percent']:.1f}%"
        )
    print(
        f"H049 performance gate {report['status']}: {compared} compared, "
        f"{len(unbaselined)} newly added, {regressions} regression(s)"
    )
    return 1 if regressions else 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    collect_parser = subparsers.add_parser("collect")
    collect_parser.add_argument("--contract", required=True)
    collect_parser.add_argument("--criterion-dir", required=True)
    collect_parser.add_argument("--output", required=True)
    collect_parser.add_argument("--runner-name", required=True)
    collect_parser.add_argument("--commit", required=True)
    collect_parser.set_defaults(func=collect)

    compare_parser = subparsers.add_parser("compare")
    compare_parser.add_argument("--baseline", required=True)
    compare_parser.add_argument("--current", required=True)
    compare_parser.add_argument("--report", required=True)
    compare_parser.add_argument("--bootstrap-if-missing", action="store_true")
    compare_parser.add_argument("--replace-baseline", action="store_true")
    compare_parser.set_defaults(func=compare)
    args = parser.parse_args()
    if args.command == "compare" and args.bootstrap_if_missing and args.replace_baseline:
        parser.error("choose bootstrap-if-missing or replace-baseline, not both")
    return args


if __name__ == "__main__":
    try:
        arguments = parse_args()
        raise SystemExit(arguments.func(arguments))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"performance gate error: {error}", file=sys.stderr)
        raise SystemExit(2)
