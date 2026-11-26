#!/usr/bin/env python3
"""Run matched transport experiments and retain every observation."""
import argparse
import hashlib
import json
import pathlib
import random
import statistics
import subprocess


def validate(record):
    if not 0 <= record["received"] <= record["submitted"]:
        raise RuntimeError("Duplicate or impossible delivery count")
    if record["expected_frames"] and not 0 <= record["late_frames"] <= record["complete_frames"] <= record["expected_frames"]:
        raise RuntimeError("Invalid frame accounting")
    if record["both_directions_wire_bytes"] < record["wire_bytes"]:
        raise RuntimeError("Total traffic omits sender traffic")


def reliable_burst_passes(records):
    return bool(records) and all(r["errors"] == 0 and r["submitted"] == 1600 and r["received"] == 1600 and 0 <= r["control_ms"] < 100 for r in records)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=pathlib.Path, required=True)
    parser.add_argument("--candidate", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--trials", type=int, default=3)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    hashes = {name: hashlib.sha256(file.read_bytes()).hexdigest() for name, file in binaries.items()}
    cases = [("control-burst", "multi"), ("bulk-burst", "multi"), ("clean", "multi"), ("clean", "single-safe"), ("clean", "multi-safe"), ("loss", "multi"), ("burst-loss", "multi"), ("decode", "multi"), ("jitter", "multi"), ("shared", "multi"), ("one-way", "multi"), ("backlog", "multi"), ("backlog", "video")]
    experiments = [(name, scenario, mode) for scenario, mode in cases for name in binaries]
    experiments += [("candidate", scenario, mode) for scenario, mode in [("loss", "loss-relaxed"), ("loss", "fec"), ("loss", "fec-budget"), ("burst-loss", "fec-budget"), ("decode", "fixed-decode"), ("decode", "arrival-off"), ("jitter", "arrival-off"), ("shared", "coupled")]]
    results = []
    with (args.output / "observations.jsonl").open("w") as out:
        for trial in range(args.trials):
            ordered = list(experiments)
            random.Random(trial).shuffle(ordered)
            for implementation, scenario, mode in ordered:
                run = subprocess.run([str(binaries[implementation]), scenario, mode, str(trial + 1)], capture_output=True, text=True, timeout=20, check=True)
                record = json.loads(run.stdout)
                record.update(implementation=implementation, binary_sha256=hashes[implementation], trial=trial, path_capacity_bps=12_000_000, shared_capacity_bps=12_000_000 if scenario == "shared" else None, traffic_accounting="both directions, UDP payload plus 28-byte IPv4/UDP header; excludes Ethernet")
                validate(record)
                results.append(record)
                out.write(json.dumps(record) + "\n")
                out.flush()
                print(json.dumps(record), flush=True)
    summary = []
    for implementation, scenario, mode in experiments:
        rows = [r for r in results if (r["implementation"], r["scenario"], r["mode"]) == (implementation, scenario, mode)]
        summary.append(dict(implementation=implementation, scenario=scenario, mode=mode, trials=len(rows), median_timely_frames=statistics.median(r["complete_frames"]-r["late_frames"] for r in rows), median_final_bitrate=statistics.median(r["final_bitrate"] for r in rows), median_wire_bytes=statistics.median(r["both_directions_wire_bytes"] for r in rows), median_control_ms=statistics.median(r["control_ms"] for r in rows)))
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2))
    candidate_burst = [r for r in results if r["implementation"] == "candidate" and r["scenario"] == "control-burst"]
    unchanged = [r for r in results if r["implementation"] == "baseline" and r["scenario"] == "control-burst"]
    if reliable_burst_passes(unchanged):
        raise RuntimeError("The unchanged transport was not rejected by the reliable-burst gate")
    if not reliable_burst_passes(candidate_burst):
        raise RuntimeError("Candidate did not preserve keyframe-burst delivery and responsive controls")
    print("Reliable-burst gate passed and rejected the unchanged implementation. Other experiments require comparison of delivery, bitrate and traffic costs; this gate does not certify hardware playback or perceived image quality.", flush=True)


if __name__ == "__main__":
    main()
