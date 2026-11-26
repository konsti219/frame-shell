// SPDX-License-Identifier: GPL-3.0-or-later
#include "multilink.h"

#include <algorithm>
#include <chrono>
#include <iostream>
#include <map>
#include <net/if.h>
#include <stdexcept>
#include <string_view>
#include <thread>

using namespace wivrn;
using namespace std::chrono_literals;
using test_clock = std::chrono::steady_clock;

static void require(bool condition, const char * message)
{
	if (not condition)
		throw std::runtime_error(message);
}

static std::map<std::string, uint64_t> video_counts(const multilink & transport)
{
	std::map<std::string, uint64_t> result;
	for (const auto & link: transport.statistics())
		result[link.local.interface] += link.received_video_packets;
	return result;
}

int main(int argc, char ** argv)
try
{
	if (argc != 11)
	{
		std::cerr << "Usage: test-multilink-live server|client local1 interface1 local2 interface2 peer1 peer-interface1 peer2 peer-interface2 key-hex\n"
		          << "Run on both hosts with the same random 32-byte key. Server listens on UDP 9757.\n";
		return 2;
	}
	const bool server = std::string_view(argv[1]) == "server";
	require(server or std::string_view(argv[1]) == "client", "Invalid role");
	require(std::string_view(argv[3]) != argv[5], "Two distinct local interfaces are required");
	require(if_nametoindex(argv[3]) != 0 and if_nametoindex(argv[5]) != 0, "Unknown local interface");
	const std::string hex = argv[10];
	require(hex.size() == 64 and hex.find_first_not_of("0123456789abcdefABCDEF") == std::string::npos, "Invalid session key");
	multilink::key_type key;
	for (size_t i = 0; i < key.size(); ++i)
		key[i] = std::stoul(hex.substr(i * 2, 2), nullptr, 16);
	const uint16_t local_port = server ? 9757 : 0;
	const uint16_t peer_port = server ? 0 : 9757;
	std::vector<network_endpoint> local{{argv[2], argv[3], local_port}, {argv[4], argv[5], local_port}};
	std::vector<network_endpoint> remote;
	if (not server)
		remote = {{argv[6], argv[7], peer_port}, {argv[8], argv[9], peer_port}};
	multilink transport(server, key, local, remote, false);
	require(transport.endpoints().size() == 2, "Could not bind both interfaces");
	const auto print_links = [&] {
		for (const auto & link: transport.statistics())
			std::cout << "link " << link.local.interface << ' ' << link.remote.address << " active=" << link.active
			          << " rtt_ms=" << link.rtt_ms << " video_packets=" << link.received_video_packets
			          << " sent_bytes=" << link.sent_bytes << " received_bytes=" << link.received_bytes << '\n';
		std::cout << std::flush;
	};
	const auto ready_deadline = test_clock::now() + 10s;
	auto stable_since = test_clock::now();
	while (test_clock::now() - stable_since < 1s)
	{
		std::map<std::string, bool> active;
		for (const auto & link: transport.statistics())
			active[link.local.interface] |= link.active;
		if (not active[argv[3]] or not active[argv[5]])
			stable_since = test_clock::now();
		if (test_clock::now() > ready_deadline)
		{
			print_links();
			throw std::runtime_error("Both interfaces must stay active together for one second");
		}
		std::this_thread::sleep_for(10ms);
	}
	std::cout << "Both interfaces active; sending 32 Mbit/s of synthetic video in each direction\n"
	          << std::flush;
	const std::vector<uint8_t> payload(1000, server ? 0x53 : 0x43);
	const std::vector<uint8_t> expected(1000, server ? 0x43 : 0x53);
	const auto start = test_clock::now();
	auto next = start;
	auto sample = start;
	auto previous = video_counts(transport);
	unsigned concurrent_seconds = 0;
	uint64_t received = 0;
	while (test_clock::now() - start < 9s)
	{
		const auto now = test_clock::now();
		if (now - start < 8s and now >= next)
		{
			serialization_packet packet;
			packet.serialize(payload);
			transport.send_video_raw(std::move(packet), now + 100ms);
			next = std::max(next + 250us, now);
		}
		for (auto packet = transport.receive_raw(); not packet.empty(); packet = transport.receive_raw())
		{
			require(packet.deserialize<std::vector<uint8_t>>() == expected, "Corrupted payload");
			++received;
		}
		if (now - sample >= 1s)
		{
			const auto current = video_counts(transport);
			const auto first = current.at(argv[3]) - previous[argv[3]];
			const auto second = current.at(argv[5]) - previous[argv[5]];
			std::cout << "sample " << argv[3] << '=' << first << ' ' << argv[5] << '=' << second << '\n'
			          << std::flush;
			concurrent_seconds += first >= 100 and second >= 100;
			previous = current;
			sample = now;
		}
		std::this_thread::sleep_for(50us);
	}
	print_links();
	std::cout << "received_messages=" << received << " concurrent_seconds=" << concurrent_seconds << '\n';
	require(received >= 10'000, "Insufficient delivered video");
	require(concurrent_seconds >= 5, "Both interfaces must deliver video in at least five one-second windows");
	std::cout << "Concurrent two-interface video delivery passed\n";
}
catch (const std::exception & error)
{
	std::cerr << error.what() << '\n';
	return 1;
}
