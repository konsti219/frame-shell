#!/usr/bin/env python3
"""Compare complete synthetic frames over real links; retain timing uncertainty."""

import argparse
import hashlib
import json
import math
import pathlib
import random
import secrets
import shlex
import statistics
import subprocess
import time


def records(file):
    return [json.loads(line) for line in file.read_text().splitlines() if line.startswith("{")]


def percentile(values, fraction):
    return (
        sorted(values)[min(len(values) - 1, math.ceil(len(values) * fraction) - 1)]
        if values
        else None
    )


def summarize(sender, receiver):
    config = next(row for row in sender if row["type"] == "config")
    if config != next(row for row in receiver if row["type"] == "config"):
        raise RuntimeError("Sender and receiver workload settings differ")
    sync = [row for row in sender if row["type"] == "sync"]
    if len(sync) < 40:
        raise RuntimeError("Insufficient clock samples before and after the workload")
    bounds = []
    for group in (sync[:30], sync[30:]):
        best = min(group, key=lambda row: (row["t4"] - row["t1"]) - (row["t3"] - row["t2"]))
        lower, upper = best["t3"] - best["t4"], best["t2"] - best["t1"]
        if lower > upper:
            raise RuntimeError("Impossible clock interval")
        bounds.append((lower, upper))
    # Union retains uncertainty from both endpoint measurements, including observed drift.
    lower = min(bound[0] for bound in bounds)
    upper = max(bound[1] for bound in bounds)
    midpoint = (lower + upper) / 2
    frame_rows = [row for row in receiver if row["type"] == "frame"]
    seen = set()
    latencies = []
    on_time_min = on_time_max = 0
    deadline = config["deadline_ms"] * 1_000_000
    for row in frame_rows:
        frame = row["frame"]
        if (
            frame in seen
            or not config["warmup_frames"]
            <= frame
            < config["warmup_frames"] + config["measured_frames"]
        ):
            raise RuntimeError("Duplicate or out-of-range completed frame")
        seen.add(frame)
        raw = row["completed_ns"] - row["born_ns"]
        if raw - lower < 0:
            raise RuntimeError("Completion predates frame production even with clock uncertainty")
        latencies.append((raw - midpoint) / 1_000_000)
        on_time_min += raw - lower <= deadline
        on_time_max += raw - upper <= deadline
    result = next(row for row in receiver if row["type"] == "receiver_summary")
    sent = next(row for row in sender if row["type"] == "sender_summary")
    expected_shards = (config["measured_frames"] + config["warmup_frames"]) * config["parts"]
    if (
        result["complete_frames"] != len(seen)
        or sent["accepted_shards"]
        + sent["rejected_shards"]
        + sent.get("deliberately_omitted_shards", 0)
        != expected_shards
    ):
        raise RuntimeError("Frame or offered-workload accounting mismatch")
    if result["payload_bytes"] > config["frame_bytes"] * config["measured_frames"]:
        raise RuntimeError("Delivered more application payload than offered")
    links = [row for row in sender + receiver if row["type"] == "link"]
    datagram_bytes = sum(row["sent_bytes"] for row in links)
    return {
        "mode": config["mode"],
        "bitrate": config["bitrate"],
        "frames": config["measured_frames"],
        "complete_frames": len(seen),
        "missing_frames": config["measured_frames"] - len(seen),
        "on_time_min": on_time_min,
        "on_time_max": on_time_max,
        "ambiguous_frames": on_time_max - on_time_min,
        "completion_p50_ms": statistics.median(latencies) if latencies else None,
        "completion_p95_ms": percentile(latencies, 0.95),
        "clock_uncertainty_ms": (upper - lower) / 2_000_000,
        "clock_endpoint_midpoint_drift_ms": abs(sum(bounds[0]) - sum(bounds[1])) / 2_000_000,
        "delivered_payload_mbps": result["payload_bytes"]
        * 8
        * config["fps"]
        / config["measured_frames"]
        / 1_000_000,
        "transport_datagram_bytes_both_directions": datagram_bytes,
        "datagram_to_unique_payload_ratio": datagram_bytes / result["total_payload_bytes"]
        if result["total_payload_bytes"]
        else None,
        "max_producer_late_ms": sent["max_producer_late_ns"] / 1_000_000,
        "expired_video": sent["expired_video"],
        "rejected_shards": sent["rejected_shards"],
        "deliberately_omitted_shards": sent.get("deliberately_omitted_shards", 0),
        "links": links,
    }


def measurement_controls():
    config = {
        "type": "config",
        "mode": "control",
        "bitrate": 8000,
        "fps": 1,
        "measured_frames": 2,
        "warmup_frames": 0,
        "deadline_ms": 20,
        "frame_bytes": 1000,
        "parts": 1,
    }
    sync = [{"type": "sync", "t1": 0, "t2": 1_000_000, "t3": 1_000_000, "t4": 2_000_000}] * 60
    sender = [
        config,
        *sync,
        {
            "type": "sender_summary",
            "accepted_shards": 2,
            "rejected_shards": 0,
            "max_producer_late_ns": 0,
            "expired_video": 0,
        },
    ]
    received = [
        config,
        {"type": "frame", "frame": 0, "born_ns": 0, "completed_ns": 5_000_000},
        {
            "type": "receiver_summary",
            "complete_frames": 1,
            "payload_bytes": 1000,
            "total_payload_bytes": 1000,
        },
    ]
    good = summarize(sender, received)
    if (good["on_time_min"], good["missing_frames"]) != (1, 1):
        raise RuntimeError("Measurement missed the known missing frame")
    delayed = [
        dict(row, completed_ns=30_000_000) if row["type"] == "frame" else row for row in received
    ]
    if summarize(sender, delayed)["on_time_max"] != 0:
        raise RuntimeError("Measurement accepted a known late frame")
    duplicate = [*received, received[1]]
    try:
        summarize(sender, duplicate)
    except RuntimeError:
        return
    raise RuntimeError("Measurement accepted duplicate completion")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--candidate", type=pathlib.Path, required=True)
    parser.add_argument("--remote-binary", required=True)
    parser.add_argument("--host", default="steamos@frame")
    parser.add_argument("--pc-wifi", nargs=2, required=True)
    parser.add_argument("--pc-direct", nargs=2, required=True)
    parser.add_argument("--frame-wifi", nargs=2, required=True)
    parser.add_argument("--frame-direct", nargs=2, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--rates", type=int, nargs="+", default=[80, 160])
    parser.add_argument("--modes", nargs="+", default=["wifi", "direct", "multi"])
    parser.add_argument("--frames", type=int, default=720)
    parser.add_argument("--fps", type=int, default=90)
    parser.add_argument("--deadline-ms", type=int, default=20)
    parser.add_argument("--omit-last-shard", action="store_true")
    args = parser.parse_args()
    measurement_controls()
    args.output.mkdir(parents=True, exist_ok=False)
    binary = args.candidate.resolve()
    metadata = vars(args).copy()
    metadata.update(
        candidate=str(binary),
        output=str(args.output),
        binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
        timing="Receiver completion minus scheduled sender production; clock-offset bounds from minimum-RTT exchanges before and after each run. Assumes stable monotonic clock rates between those exchanges.",
        traffic="Transport UDP payload bytes in both directions, including warmup and control; excludes IP, UDP, Wi-Fi and Ethernet headers.",
        limitations="Synthetic frame shards; no encoder, decoder, compositor, tracking, movement or controlled RF interference. Latency percentiles include completed frames only; missing frames are reported separately.",
    )
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2))
    results = []
    for trial in range(args.trials):
        cases = [(mode, rate) for mode in args.modes for rate in args.rates]
        random.Random(37 + trial).shuffle(cases)
        for mode, rate in cases:
            stem = f"{trial}-{mode}-{rate}"
            key = secrets.token_hex(32)
            common = [mode, str(rate), str(args.fps), str(args.frames), str(args.deadline_ms), key]
            receiver_args = [
                args.remote_binary,
                "server",
                *common,
                *args.frame_wifi,
                *args.frame_direct,
                *args.pc_wifi,
                *args.pc_direct,
            ]
            sender_args = [
                str(binary),
                "client",
                *common,
                *args.pc_wifi,
                *args.pc_direct,
                *args.frame_wifi,
                *args.frame_direct,
            ]
            if args.omit_last_shard:
                receiver_args.append("omit-last-shard")
                sender_args.append("omit-last-shard")
            receiver_file = args.output / f"{stem}-receiver.jsonl"
            sender_file = args.output / f"{stem}-sender.jsonl"
            with receiver_file.open("w") as remote_log, sender_file.open("w") as local_log:
                receiver = subprocess.Popen(
                    [
                        "ssh",
                        "-o",
                        "BatchMode=yes",
                        "-o",
                        "ConnectTimeout=8",
                        args.host,
                        shlex.join(receiver_args),
                    ],
                    stdout=remote_log,
                    stderr=subprocess.STDOUT,
                )
                try:
                    time.sleep(0.5)
                    sender = subprocess.run(
                        sender_args,
                        stdout=local_log,
                        stderr=subprocess.STDOUT,
                        timeout=args.frames / args.fps + 20,
                        check=False,
                    )
                    receiver_code = receiver.wait(timeout=20)
                except BaseException:
                    receiver.terminate()
                    receiver.wait(timeout=5)
                    raise
            if sender.returncode or receiver_code:
                raise RuntimeError(
                    f"{stem}: sender={sender.returncode}, receiver={receiver_code}; inspect retained logs"
                )
            result = summarize(records(sender_file), records(receiver_file))
            if args.omit_last_shard:
                if result["complete_frames"] or result["on_time_max"]:
                    raise RuntimeError(
                        "Measurement accepted a frame with an intentionally missing shard"
                    )
                result["missing_shard_control_rejected"] = True
            result.update(trial=trial, stem=stem)
            results.append(result)
            (args.output / "results.json").write_text(json.dumps(results, indent=2))
            print(
                json.dumps({key: value for key, value in result.items() if key != "links"}),
                flush=True,
            )


if __name__ == "__main__":
    main()
