"""Compare host JSONL IO against a Git revision on synthetic local data.

Run with the bridge virtualenv from the repository root, for example:
    bridge/.venv/bin/python bridge/tools/benchmark_io.py --baseline 774a9f1

Loads audit.py and jsonl_tailer.py from the supplied local revision as Python
modules. Review that revision before running. No service, board or API is used.
Timing includes tracemalloc overhead; peaks are Python allocations, not RSS.
"""

from __future__ import annotations

import argparse
import asyncio
import builtins
import json
import statistics
import subprocess
import tempfile
import time
import tracemalloc
import types
from pathlib import Path
from unittest.mock import patch

from eve_bridge import audit, jsonl_tailer

REPOSITORY = Path(__file__).resolve().parents[2]


def revision_module(name: str, revision: str) -> types.ModuleType:
    source = subprocess.run(
        ["git", "-C", str(REPOSITORY), "show", f"{revision}:bridge/src/eve_bridge/{name}.py"],
        check=True, capture_output=True, text=True,
    ).stdout
    module = types.ModuleType(f"eve_bridge._benchmark_baseline_{name}")
    module.__package__ = "eve_bridge"
    exec(compile(source, f"{revision}:{name}.py", "exec"), module.__dict__)  # noqa: S102
    return module


async def no_watch(*args, **kwargs):
    return
    yield  # pragma: no cover -- async generator that ends without filesystem watching


async def no_update(*args):
    pass


class Sink:
    def isatty(self):
        return False

    def write(self, text):
        return len(text)


def benchmark_tailer(module, path: Path, records: int, runs: int) -> dict:
    times, peaks, reads = [], [], []
    real_open = builtins.open
    for _ in range(runs):
        count = 0

        def tracked_open(file, *args, **kwargs):
            nonlocal count
            if str(file) == str(path):
                count += 1
            return real_open(file, *args, **kwargs)

        tailer = module.JSONLTailer(no_update, roots=[path.parent])
        with patch.object(module, "awatch", no_watch), patch("builtins.open", tracked_open):
            tracemalloc.start()
            try:
                start = time.perf_counter()
                asyncio.run(tailer.run())
                times.append(time.perf_counter() - start)
                peaks.append(tracemalloc.get_traced_memory()[1])
            finally:
                tracemalloc.stop()
        assert tailer._tokens_per_file[str(path)] == records * 200
        assert len(tailer._emitted_assistant_uuids[str(path)]) == records
        reads.append(count)
    return {"rows": records, "file_bytes": path.stat().st_size,
            "median_seconds": statistics.median(times), "max_peak_bytes": max(peaks),
            "reads_each_run": reads, "seen_uuids": records}


def benchmark_audit(module, path: Path, records: int, runs: int) -> dict:
    times, peaks = [], []
    for _ in range(runs):
        tracemalloc.start()
        try:
            start = time.perf_counter()
            module.render(path=path, last=20, ascii_only=True, out=Sink())
            times.append(time.perf_counter() - start)
            peaks.append(tracemalloc.get_traced_memory()[1])
        finally:
            tracemalloc.stop()
    return {"rows": records, "file_bytes": path.stat().st_size,
            "median_seconds": statistics.median(times), "max_peak_bytes": max(peaks)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", default="774a9f1", help="Reviewed local Git revision to compare")
    parser.add_argument("--records", type=int, default=20_000)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--json", type=Path, help="Optional path for measured JSON results")
    args = parser.parse_args()
    if args.records < 1000 or args.runs < 1:
        parser.error("Use at least 1000 records and one run for the allocation comparison.")
    baseline = {name: revision_module(name, args.baseline) for name in ("audit", "jsonl_tailer")}
    results = {}
    with tempfile.TemporaryDirectory(prefix="eve-host-io-benchmark-") as folder:
        root = Path(folder)
        # Separate roots keep audit records out of the transcript tailer's sweep.
        transcripts = root / "transcripts"
        transcripts.mkdir()
        transcript = transcripts / "session.jsonl"
        audit_path = root / "audit.jsonl"
        with transcript.open("w") as file:
            for i in range(args.records):
                file.write(json.dumps({"uuid": f"{i:08d}-0000-4000-8000-000000000000",
                                       "timestamp": "2026-10-03T12:00:00Z",
                                       "message": {"role": "assistant", "model": "claude-sonnet-4-6",
                                                   "content": [{"type": "text", "text": "x" * 200}],
                                                   "usage": {"input_tokens": 1000, "output_tokens": 200}}}) + "\n")
        with audit_path.open("w") as file:
            for i in range(args.records):
                file.write(json.dumps({"tool": "Bash", "hint": str(i) + "x" * 200}) + "\n")
        for label, module in (("baseline", baseline["jsonl_tailer"]), ("updated", jsonl_tailer)):
            results[f"tailer_representative_{label}"] = benchmark_tailer(
                module, transcript, args.records, args.runs,
            )
        for label, module in (("baseline", baseline["audit"]), ("updated", audit)):
            results[f"audit_{label}"] = benchmark_audit(module, audit_path, args.records, args.runs)
    old, new = results["tailer_representative_baseline"], results["tailer_representative_updated"]
    assert old["reads_each_run"] == [2] * args.runs, old
    assert new["reads_each_run"] == [1] * args.runs, new
    assert new["max_peak_bytes"] < old["max_peak_bytes"] * 0.7, (old, new)
    old, new = results["audit_baseline"], results["audit_updated"]
    assert new["max_peak_bytes"] < old["max_peak_bytes"] * 0.1, (old, new)
    encoded = json.dumps({"baseline_revision": args.baseline,
                          "scope": "Synthetic local records; timings include tracemalloc; Python allocations, not RSS",
                          "measurements": results}, indent=2) + "\n"
    if args.json:
        args.json.write_text(encoded)
    print(encoded, end="")
    print("HOST_IO_BENCHMARK_OK")


if __name__ == "__main__":
    main()
