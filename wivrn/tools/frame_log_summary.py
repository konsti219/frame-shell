#!/usr/bin/env python3
"""Summarize WIVRN_FRAME_LOG files: lost frames, freezes and latency of the VR stream."""

import argparse
import itertools
import json
import math
import pathlib

EYES = (0, 1)
FREEZE_NS = 50_000_000
# Frames sent this close to the end of a log may still have their feedback in flight
TAIL_NS = 1_000_000_000


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, math.ceil(len(ordered) * fraction) - 1)]


def summarize(rows):
    sent = {}
    frames = {}
    network = []
    for row in rows:
        kind = row["type"]
        if kind == "network":
            network.append(row)
            continue
        if row["stream"] not in EYES:
            continue
        key = (row["stream"], row["frame"])
        if kind == "sent":
            sent[key] = row
        elif kind == "feedback":
            frame = frames.setdefault(
                key, {"times_displayed": 0, "displayed": None, "complete": False, "resent": False}
            )
            frame["complete"] |= row["received_last_packet"] != 0
            frame["resent"] |= row["resent"]
            if row["times_displayed"]:
                frame["times_displayed"] = max(frame["times_displayed"], row["times_displayed"])
                if frame["displayed"] is None or row["displayed"] < frame["displayed"]:
                    frame["displayed"] = row["displayed"]
                    frame["received_last_packet"] = row["received_last_packet"]
                    frame["encode_begin"] = row["encode_begin"]
    if not sent:
        raise RuntimeError("No VR frames sent in the log")

    shown = {key: frame for key, frame in frames.items() if frame["displayed"] is not None}
    if not shown:
        raise RuntimeError("No VR frame was displayed")
    first = min(frame["displayed"] for frame in shown.values())
    last_send = max(row["send_end"] for row in sent.values())
    counted = [
        key
        for key, row in sent.items()
        if first <= row["encode_begin"] and row["send_end"] <= last_send - TAIL_NS
    ]
    lost = [key for key in counted if key not in shown]
    incomplete = [key for key in lost if not frames.get(key, {}).get("complete")]

    freezes = {}
    longest = {}
    for eye in EYES:
        times = sorted(frame["displayed"] for (stream, _), frame in shown.items() if stream == eye)
        gaps = [later - earlier for earlier, later in itertools.pairwise(times)]
        freezes[eye] = sum(gap > FREEZE_NS for gap in gaps)
        longest[eye] = max(gaps, default=0)

    latency = [(frame["displayed"] - frame["encode_begin"]) / 1e6 for frame in shown.values()]
    transfer = [
        (frame["received_last_packet"] - sent[key]["send_begin"]) / 1e6
        for key, frame in shown.items()
        if key in sent and frame["received_last_packet"]
    ]
    duration_s = (max(frame["displayed"] for frame in shown.values()) - first) / 1e9

    link_seconds = {}
    link_sets = []
    for row in network:
        used = tuple(sorted(link["local"] for link in row["links"] if link["sent_bps"] > 0))
        for name in used:
            link_seconds[name] = link_seconds.get(name, 0) + 1
        if not link_sets or link_sets[-1] != used:
            link_sets.append(used)

    return {
        "duration_s": round(duration_s, 1),
        "frames_sent": len(counted),
        "frames_lost": len(lost),
        "lost_percent": round(100 * len(lost) / len(counted), 3) if counted else None,
        "lost_incomplete": len(incomplete),
        "frames_resent": sum(frame["resent"] for key, frame in shown.items() if key in sent),
        "repeated_displays": sum(
            frame["times_displayed"] - 1 for (stream, _), frame in shown.items() if stream == 0
        ),
        "freezes_over_50ms": max(freezes.values()),
        "freezes_per_minute": round(max(freezes.values()) * 60 / duration_s, 2)
        if duration_s
        else None,
        "longest_gap_ms": round(max(longest.values()) / 1e6, 1),
        "latency_ms": {
            name: round(percentile(latency, q), 1)
            for name, q in (("p50", 0.5), ("p95", 0.95), ("p99", 0.99))
        },
        "transfer_ms": {
            name: round(percentile(transfer, q), 1)
            for name, q in (("p50", 0.5), ("p95", 0.95), ("p99", 0.99))
        }
        if transfer
        else None,
        "mean_bitrate_mbit": round(sum(row["bitrate"] for row in network) / len(network) / 1e6, 1)
        if network
        else None,
        "link_seconds": link_seconds,
        "link_changes": max(len(link_sets) - 1, 0),
    }


def self_test():
    """Known inputs: one frame lost, one 61 ms gap, one repeated display."""
    period = 11_111_111
    rows = []
    for frame in range(400):
        encode = frame * period
        rows.append(
            {
                "type": "sent",
                "stream": 0,
                "frame": frame,
                "encode_begin": encode,
                "encode_end": encode + 3_000_000,
                "send_begin": encode + 3_000_000,
                "send_end": encode + 4_000_000,
                "bytes": 1000,
            }
        )
        if frame == 100:
            continue
        # A 50 ms stall from frame 350 on
        shown_at = encode + 20_000_000 + (50_000_000 if frame >= 350 else 0)
        times = 2 if frame == 300 else 1
        rows.append(
            {
                "type": "feedback",
                "stream": 0,
                "frame": frame,
                "encode_begin": encode,
                "send_end": encode + 4_000_000,
                "received_last_packet": encode + 6_000_000,
                "displayed": shown_at,
                "times_displayed": times,
                "resent": False,
            }
        )
    result = summarize(rows)
    expected = {"frames_lost": 1, "freezes_over_50ms": 1, "repeated_displays": 1}
    if any(result[key] != value for key, value in expected.items()):
        raise RuntimeError(f"Self-test failed: {result}")
    if result["latency_ms"]["p50"] != 20.0:
        raise RuntimeError(f"Self-test latency wrong: {result['latency_ms']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "logs", type=pathlib.Path, nargs="+", help="one WIVRN_FRAME_LOG file per run"
    )
    parser.add_argument("--json", action="store_true", help="print one JSON object per run")
    args = parser.parse_args()
    self_test()
    for log in args.logs:
        rows = [json.loads(line) for line in log.read_text().splitlines() if line.startswith("{")]
        result = {"run": log.stem, **summarize(rows)}
        if args.json:
            print(json.dumps(result), flush=True)
            continue
        latency, transfer = result["latency_ms"], result["transfer_ms"] or {}
        print(
            f"{result['run']}: {result['duration_s']} s, {result['frames_lost']}/{result['frames_sent']} lost "
            f"({result['lost_percent']}%, {result['lost_incomplete']} incomplete), {result['repeated_displays']} repeated, "
            f"{result['freezes_over_50ms']} freezes > 50 ms ({result['freezes_per_minute']}/min, longest {result['longest_gap_ms']} ms), "
            f"latency p50/p95/p99 {latency['p50']}/{latency['p95']}/{latency['p99']} ms, "
            f"transfer p50/p95/p99 {transfer.get('p50')}/{transfer.get('p95')}/{transfer.get('p99')} ms, "
            f"bitrate {result['mean_bitrate_mbit']} Mbit/s, links {result['link_seconds']}, {result['link_changes']} link changes"
        )


if __name__ == "__main__":
    main()
