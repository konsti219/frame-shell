// SPDX-License-Identifier: GPL-3.0-or-later
#include "multilink.h"

#include <algorithm>
#include <chrono>
#include <iostream>
#include <map>
#include <poll.h>
#include <set>
#include <stdexcept>
#include <string_view>
#include <thread>

using namespace wivrn;
using namespace std::chrono_literals;
using bench_clock = std::chrono::steady_clock;

enum : uint8_t
{
	sync_request,
	sync_reply,
	video_shard,
	finish
};

struct message
{
	uint8_t type;
	uint64_t frame = 0;
	uint32_t shard = 0, parts = 0;
	int64_t t1 = 0, t2 = 0, t3 = 0;
	std::vector<uint8_t> data;
};

static int64_t timestamp()
{
	return std::chrono::duration_cast<std::chrono::nanoseconds>(bench_clock::now().time_since_epoch()).count();
}

static void require(bool value, const char * reason)
{
	if (not value)
		throw std::runtime_error(reason);
}

static serialization_packet packet(const message & value)
{
	serialization_packet result;
	result.serialize(value);
	return result;
}

static void print_stats(const multilink & transport)
{
	for (const auto & link: transport.statistics())
		std::cout << "{\"type\":\"link\",\"interface\":\"" << link.local.interface << "\",\"peer\":\"" << link.remote.address
		          << "\",\"active\":" << link.active << ",\"rtt_ms\":" << link.rtt_ms << ",\"jitter_ms\":" << link.jitter_ms
		          << ",\"loss\":" << link.loss << ",\"capacity_bps\":" << link.capacity_bps << ",\"sent_bytes\":" << link.sent_bytes
		          << ",\"received_bytes\":" << link.received_bytes << ",\"video_packets\":" << link.received_video_packets
		          << ",\"repairs_sent\":" << link.repairs_sent << "}\n";
}

int main(int argc, char ** argv)
try
{
	if (argc != 16 and argc != 17)
	{
		std::cerr << "Usage: benchmark-multilink-live server|client wifi|direct|multi|arrival-off Mbps fps frames deadline-ms key-hex local1 interface1 local2 interface2 peer1 peer-interface1 peer2 peer-interface2 [omit-last-shard]\n";
		return 2;
	}
	const bool server = std::string_view(argv[1]) == "server";
	require(server or std::string_view(argv[1]) == "client", "Invalid role");
	const std::string mode = argv[2];
	const bool omit_last_shard = argc == 17;
	require(not omit_last_shard or std::string_view(argv[16]) == "omit-last-shard", "Invalid fault control");
	require(mode == "wifi" or mode == "direct" or mode == "multi" or mode == "arrival-off", "Invalid mode");
	const uint64_t bitrate = std::stoull(argv[3]) * 1'000'000;
	const unsigned fps = std::stoul(argv[4]);
	const unsigned measured_frames = std::stoul(argv[5]);
	const unsigned deadline_ms = std::stoul(argv[6]);
	require(bitrate > 0 and bitrate <= 1'000'000'000 and fps > 0 and fps <= 240 and measured_frames > 0 and measured_frames <= 10'000 and deadline_ms > 0 and deadline_ms <= 25, "Invalid workload");
	const std::string hex = argv[7];
	require(hex.size() == 64 and hex.find_first_not_of("0123456789abcdefABCDEF") == std::string::npos, "Invalid key");
	multilink::key_type key;
	for (size_t i = 0; i < key.size(); ++i)
		key[i] = std::stoul(hex.substr(i * 2, 2), nullptr, 16);
	const uint16_t port = server ? 9757 : 0;
	std::vector<network_endpoint> local{{argv[8], argv[9], port}, {argv[10], argv[11], port}};
	std::vector<network_endpoint> peers{{argv[12], argv[13], 9757}, {argv[14], argv[15], 9757}};
	if (mode == "wifi")
	{
		local.resize(1);
		peers.resize(1);
	}
	else if (mode == "direct")
	{
		local.erase(local.begin());
		peers.erase(peers.begin());
	}
	multilink transport(server, key, local, server ? std::vector<network_endpoint>{} : peers, false);
	transport.set_options({.arrival_scheduling = mode != "arrival-off"});
	require(transport.endpoints().size() == local.size(), "Could not bind requested interfaces");
	const size_t frame_bytes = bitrate / fps / 8;
	const uint32_t parts = (frame_bytes + 999) / 1000;
	const unsigned warmup_frames = fps * 2;
	const unsigned total_frames = measured_frames + warmup_frames;
	std::cout << "{\"type\":\"config\",\"mode\":\"" << mode << "\",\"bitrate\":" << bitrate << ",\"fps\":" << fps
	          << ",\"measured_frames\":" << measured_frames << ",\"warmup_frames\":" << warmup_frames
	          << ",\"deadline_ms\":" << deadline_ms << ",\"frame_bytes\":" << frame_bytes << ",\"parts\":" << parts << "}\n"
	          << std::flush;
	if (server)
	{
		struct frame_state
		{
			std::vector<bool> seen;
			unsigned count = 0;
			int64_t born = 0;
		};
		std::map<uint64_t, frame_state> frames;
		unsigned complete = 0;
		uint64_t payload_bytes = 0, total_payload_bytes = 0;
		auto stop = bench_clock::now() + 45s;
		bool finishing = false;
		while (bench_clock::now() < stop)
		{
			pollfd event{.fd = transport.get_fd(), .events = POLLIN};
			poll(&event, 1, 1);
			for (auto incoming = transport.receive_raw(); not incoming.empty(); incoming = transport.receive_raw())
			{
				const int64_t received = timestamp();
				auto value = incoming.deserialize<message>();
				if (value.type == sync_request)
				{
					value.type = sync_reply;
					value.t2 = received;
					value.t3 = timestamp();
					transport.send_reply_raw(packet(value));
				}
				else if (value.type == finish)
				{
					stop = bench_clock::now() + 500ms;
					finishing = true;
				}
				else if (value.type == video_shard)
				{
					require(value.frame < total_frames and value.parts == parts and value.shard < parts, "Invalid frame metadata");
					const size_t expected_size = std::min<size_t>(1000, frame_bytes - value.shard * 1000);
					require(value.data.size() == expected_size, "Wrong shard size");
					for (size_t j = 0; j < value.data.size(); ++j)
						require(value.data[j] == (value.frame + value.shard + j) % 251, "Corrupted shard");
					auto & state = frames[value.frame];
					if (state.seen.empty())
					{
						state.seen.resize(parts);
						state.born = value.t1;
					}
					require(state.born == value.t1 and not state.seen[value.shard], "Duplicate shard or inconsistent frame time");
					state.seen[value.shard] = true;
					++state.count;
					total_payload_bytes += value.data.size();
					if (value.frame >= warmup_frames)
						payload_bytes += value.data.size();
					if (state.count == parts and value.frame >= warmup_frames)
					{
						++complete;
						std::cout << "{\"type\":\"frame\",\"frame\":" << value.frame << ",\"born_ns\":" << value.t1 << ",\"completed_ns\":" << received << "}\n";
					}
				}
			}
		}
		print_stats(transport);
		std::cout << "{\"type\":\"receiver_summary\",\"complete_frames\":" << complete << ",\"payload_bytes\":" << payload_bytes << ",\"total_payload_bytes\":" << total_payload_bytes << ",\"finished\":" << finishing << "}\n";
		require(finishing, "Sender did not finish");
	}
	else
	{
		const auto ready_timeout = bench_clock::now() + 8s;
		while (true)
		{
			std::set<std::string> active;
			for (const auto & link: transport.statistics())
				if (link.active)
					active.insert(link.local.interface);
			if (active.size() == local.size())
				break;
			require(bench_clock::now() < ready_timeout, "Paths did not validate");
			std::this_thread::sleep_for(10ms);
		}
		const auto receive = [&] {
			for (auto incoming = transport.receive_raw(); not incoming.empty(); incoming = transport.receive_raw())
			{
				const int64_t t4 = timestamp();
				const auto value = incoming.deserialize<message>();
				require(value.type == sync_reply, "Unexpected reply");
				std::cout << "{\"type\":\"sync\",\"t1\":" << value.t1 << ",\"t2\":" << value.t2 << ",\"t3\":" << value.t3 << ",\"t4\":" << t4 << "}\n";
			}
		};
		const auto synchronize = [&] {
			for (unsigned i = 0; i < 30; ++i)
			{
				message request{.type = sync_request, .t1 = timestamp()};
				transport.send_raw(packet(request));
				const auto until = bench_clock::now() + 10ms;
				while (bench_clock::now() < until)
				{
					receive();
					std::this_thread::sleep_for(100us);
				}
			}
		};
		synchronize();
		const auto start = bench_clock::now() + 20ms;
		uint64_t accepted = 0, rejected = 0, measured_accepted = 0, deliberately_omitted = 0;
		int64_t max_producer_late_ns = 0;
		for (uint64_t frame = 0; frame < total_frames; ++frame)
		{
			const auto due = start + std::chrono::nanoseconds(frame * 1'000'000'000 / fps);
			std::this_thread::sleep_until(due);
			const int64_t born = std::chrono::duration_cast<std::chrono::nanoseconds>(due.time_since_epoch()).count();
			if (frame >= warmup_frames)
				max_producer_late_ns = std::max(max_producer_late_ns, timestamp() - born);
			for (uint32_t shard = 0; shard < parts; ++shard)
			{
				if (omit_last_shard and shard + 1 == parts)
				{
					++deliberately_omitted;
					continue;
				}
				message value{.type = video_shard, .frame = frame, .shard = shard, .parts = parts, .t1 = born};
				value.data.resize(std::min<size_t>(1000, frame_bytes - shard * 1000));
				for (size_t j = 0; j < value.data.size(); ++j)
					value.data[j] = (frame + shard + j) % 251;
				const auto sent = transport.send_video_raw(packet(value), due + std::chrono::milliseconds(deadline_ms));
				if (sent)
				{
					++accepted;
					if (frame >= warmup_frames)
						measured_accepted += value.data.size();
				}
				else
					++rejected;
			}
			receive();
		}
		std::this_thread::sleep_for(100ms);
		synchronize();
		message done{.type = finish};
		transport.send_reliable_raw(packet(done));
		std::this_thread::sleep_for(250ms);
		print_stats(transport);
		const auto diagnostic = transport.diagnostics();
		std::cout << "{\"type\":\"sender_summary\",\"accepted_shards\":" << accepted << ",\"rejected_shards\":" << rejected
		          << ",\"deliberately_omitted_shards\":" << deliberately_omitted
		          << ",\"measured_accepted_bytes\":" << measured_accepted << ",\"expired_video\":" << diagnostic.expired_video
		          << ",\"max_producer_late_ns\":" << max_producer_late_ns << "}\n";
	}
}
catch (const std::exception & error)
{
	std::cerr << error.what() << '\n';
	return 1;
}
