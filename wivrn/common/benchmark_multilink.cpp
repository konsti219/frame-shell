// SPDX-License-Identifier: GPL-3.0-or-later
#ifdef WIVRN_BENCH_BASELINE
#include WIVRN_BENCH_BASELINE_HEADER
#else
#include "multilink.h"
#endif
#include "adaptive_bitrate.h"
#include "decode_budget.h"
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <iostream>
#include <map>
#include <mutex>
#include <poll.h>
#include <source_location>
#include <thread>
#include <unistd.h>

using namespace wivrn;
using namespace std::chrono_literals;
using bench_clock = std::chrono::steady_clock;
static void check(bool value, std::source_location at = std::source_location::current())
{
	if (not value)
	{
		std::cerr << at.file_name() << ':' << at.line() << '\n';
		std::exit(2);
	}
}
#include "test_network_relay.h"

template <typename F>
static void wait_for(F condition)
{
	const auto until = bench_clock::now() + 3s;
	while (not condition())
	{
		check(bench_clock::now() < until);
		std::this_thread::sleep_for(1ms);
	}
}

static serialization_packet packet(const std::vector<uint8_t> & value)
{
	serialization_packet result;
	result.serialize(value);
	return result;
}

int main(int argc, char ** argv)
{
	const std::string scenario = argc > 1 ? argv[1] : "clean";
	const bool single = argc > 2 and std::string_view(argv[2]).starts_with("single");
	const std::string mode = argc > 2 ? argv[2] : "multi";
	const bool fec = mode.starts_with("fec");
	(void)fec;
	const uint32_t seed = argc > 3 ? std::stoul(argv[3]) : 1;
	const auto key = multilink::random_key();
	multilink receiver(true, key, {{"127.0.0.20", "a"}, {"127.0.0.21", "b"}}, {}, false);
	const auto shared = scenario == "shared" ? std::make_shared<relay::shared_budget>(12'000'000) : nullptr;
	relay first(receiver.endpoints()[0], shared, seed), second(receiver.endpoints()[1], shared, seed ^ 0x9e3779b9);
	first.rate_bps = second.rate_bps = 12'000'000;
	if (scenario == "unequal" or scenario == "decode" or scenario == "jitter")
		second.delay_ms = scenario == "decode" ? 6 : 8;
	if (scenario == "jitter")
		second.jitter_ms = 12;
	if (scenario == "loss")
		first.random_drop_per_mille = second.random_drop_per_mille = 30;
	if (scenario == "burst-loss")
	{
		first.burst_every = second.burst_every = 100;
		first.burst_length = second.burst_length = 3;
	}
	std::vector<network_endpoint> peers{first.endpoint};
	if (not single)
		peers.push_back(second.endpoint);
	multilink sender(false, key, {{"127.0.0.22", "client"}}, peers, false);
#ifndef WIVRN_BENCH_BASELINE
	sender.set_options({.arrival_scheduling = mode != "arrival-off", .fec_group_size = uint8_t(fec ? 4 : 0), .shared_bottleneck = mode == "coupled", .loss_threshold = mode == "loss-relaxed" ? 0.15 : 0.02});
#endif
	wait_for([&] { return std::ranges::count_if(sender.statistics(), [](const auto & p) { return p.active; }) == (single ? 1 : 2); });
	std::vector<uint8_t> bytes(1000, 0x39);
	decode_budget decode_timing;
	for (uint64_t i = 0; i < 64; ++i)
		decode_timing.observe(1, i, 5'000'000);
	const uint32_t desired_bitrate = mode == "fec-budget" ? 12'800'000 : scenario == "decode" or scenario == "jitter" or mode.ends_with("safe") ? 8'000'000
	                                                                                                                                            : 16'000'000;
	if (mode == "fec-budget")
		bytes.resize(800);
	uint32_t adapted_bitrate = desired_bitrate;
	const auto start = bench_clock::now();
	auto last_adaptation = start;
	auto next = start;
	uint64_t submitted = 0, received = 0, complete = 0, late = 0, errors = 0;
	uint64_t frame_index = 0, payload_delivered = 0;
	uint16_t shard_index = 0, frame_parts = 0;
	size_t frame_remaining = 0;
	std::map<uint64_t, std::vector<std::vector<uint8_t>>> expected;
	double control_ms = -1;
	bench_clock::time_point control_sent{};
	std::map<uint64_t, std::vector<bool>> frames;
	const bool burst = scenario == "control-burst" or scenario == "bulk-burst";
	bool submission_failed = false;
	bool paused = false, resumed = false;
	const auto duration = burst ? 3s : 6s;
	while (bench_clock::now() - start < duration)
	{
		const auto now = bench_clock::now();
		if (scenario == "backlog" and not paused and now - start >= 100ms)
		{
			sender.prepare_fork();
			paused = true;
		}
		if (paused and not resumed and now - start >= 140ms)
		{
			sender.resume_after_fork();
			resumed = true;
		}
		if (scenario == "one-way" and now - start > 2s)
			second.drop_from_server = true;
		if (burst)
		{
			bytes.resize(scenario == "bulk-burst" ? 1024 * 1024 : 1400);
			const size_t count = scenario == "bulk-burst" ? 20 : 1600;
			if (submitted < count and not submission_failed)
			{
				try
				{
#ifdef WIVRN_BENCH_BASELINE
					sender.send_reliable_raw(packet(bytes));
#else
					if (scenario == "bulk-burst")
						sender.send_bulk_raw(packet(bytes));
					else
						sender.send_video_raw(packet(bytes), now + 25ms, true);
#endif
					++submitted;
					if (submitted == (scenario == "bulk-burst" ? 10 : 800))
					{
						control_sent = bench_clock::now();
						const std::vector<uint8_t> command(16, 0x7c);
						sender.send_reliable_raw(packet(command));
					}
				}
				catch (const std::exception &)
				{
					++errors;
					submission_failed = true;
				}
			}
			next += 50us;
			if (submitted >= count or submission_failed)
				next = now + 1ms;
		}
		else
		{
			if (now - last_adaptation >= 500ms)
			{
				adapted_bitrate = adapted_video_bitrate(desired_bitrate, adapted_bitrate, sender.capacity_bps());
				last_adaptation = now;
			}
			const auto frame_period = scenario == "decode" or scenario == "jitter" ? 10ms : 5ms;
			if (shard_index == 0)
			{
				frame_remaining = size_t(adapted_bitrate * std::chrono::duration<double>(frame_period).count() / 8);
				frame_parts = (frame_remaining + 999) / 1000;
				expected[frame_index].reserve(frame_parts);
			}
			const auto payload = std::min<size_t>(1000, frame_remaining);
			bytes.assign(payload + 12, 0x39);
			std::memcpy(bytes.data(), &frame_index, 8);
			std::memcpy(bytes.data() + 8, &shard_index, 2);
			std::memcpy(bytes.data() + 10, &frame_parts, 2);
			expected[frame_index].push_back(bytes);
			auto reserve = 2ms;
#ifndef WIVRN_BENCH_BASELINE
			if (scenario == "decode" and mode != "fixed-decode")
				reserve = std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::nanoseconds(decode_timing.estimate()));
#endif
			if (scenario == "backlog" and mode == "video")
				sender.send_video_raw(packet(bytes), bench_clock::now() + 25ms);
			else if (scenario == "backlog")
				sender.send_raw(packet(bytes));
			else
				sender.send_video_raw(packet(bytes), start + frame_index * frame_period + (scenario == "decode" ? 18ms - reserve : 20ms));
			++submitted;
			frame_remaining -= payload;
			if (++shard_index == frame_parts)
			{
				shard_index = 0;
				next = start + ++frame_index * frame_period;
			}
			else
				next += std::chrono::nanoseconds(uint64_t(payload * 8e9 / (adapted_bitrate * 1.25)));
			if (scenario == "backlog")
				next = start + std::chrono::microseconds(submitted * 500);
		}
		for (auto incoming = receiver.receive_raw(); not incoming.empty(); incoming = receiver.receive_raw())
		{
			const auto value = incoming.deserialize<std::vector<uint8_t>>();
			if (value.size() == 16 and value.front() == 0x7c)
			{
				control_ms = std::chrono::duration<double, std::milli>(bench_clock::now() - control_sent).count();
				continue;
			}
			++received;
			if (not burst)
			{
				check(value.size() >= 12);
				uint64_t frame;
				uint16_t shard, parts;
				std::memcpy(&frame, value.data(), 8);
				std::memcpy(&shard, value.data() + 8, 2);
				std::memcpy(&parts, value.data() + 10, 2);
				check(parts > 0 and parts <= 1024 and shard < parts);
				check(expected.contains(frame) and shard < expected.at(frame).size() and expected.at(frame)[shard] == value);
				auto & seen = frames[frame];
				if (seen.empty())
					seen.resize(parts);
				check(seen.size() == parts and not seen[shard]);
				seen[shard] = true;
				payload_delivered += value.size() - 12;
				if (std::ranges::all_of(seen, [](bool value) { return value; }))
				{
					++complete;
					const auto completed = bench_clock::now() + (scenario == "decode" ? 5ms : 0ms);
					const auto period = scenario == "decode" or scenario == "jitter" ? 10ms : 5ms;
					if (completed > start + frame * period + (scenario == "decode" ? 18ms : 20ms))
						++late;
				}
			}
		}
		std::this_thread::sleep_until(next);
	}
	const auto elapsed = std::chrono::duration<double>(bench_clock::now() - start).count();
	uint64_t wire = 0;
	for (const auto & link: sender.statistics())
		wire += link.sent_bytes;
	std::cout << "{\"scenario\":\"" << scenario << "\",\"mode\":\"" << mode << "\",\"seed\":" << seed << ",\"final_bitrate\":" << adapted_bitrate << ",\"both_directions_wire_bytes\":" << first.offered_wire_bytes + second.offered_wire_bytes << ",\"fec\":" << fec << ",\"desired_bitrate\":" << desired_bitrate << ",\"coupled\":" << (mode == "coupled") << ",\"impairment_drops\":" << first.dropped_impairment + second.dropped_impairment << ",\"rate_drops\":" << first.dropped_rate + second.dropped_rate << ",\"single\":" << single << ",\"submitted\":" << submitted << ",\"payload_delivered\":" << payload_delivered << ",\"expected_frames\":" << frame_index + (shard_index != 0) << ",\"received\":" << received << ",\"complete_frames\":" << complete << ",\"late_frames\":" << late << ",\"errors\":" << errors << ",\"wire_bytes\":" << wire << ",\"seconds\":" << elapsed << ",\"control_ms\":" << control_ms << "}" << std::endl;
	if (argc > 4 and std::string_view(argv[4]) == "--verify" and scenario == "backlog")
		check(received > submitted * 0.9);
}
