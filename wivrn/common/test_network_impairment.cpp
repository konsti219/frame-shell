// SPDX-License-Identifier: GPL-3.0-or-later
#include "multilink.h"
#include "network_impairment.h"

#include <iostream>
#include <thread>

using namespace wivrn;
using namespace std::chrono_literals;
using test_clock = network_impairment::clock;

static void require(bool condition, const char * message)
{
	if (not condition)
		throw std::runtime_error(message);
}

template <typename Predicate>
static bool wait_for(Predicate predicate, std::chrono::milliseconds timeout = 1500ms)
{
	const auto until = test_clock::now() + timeout;
	do
	{
		if (predicate())
			return true;
		std::this_thread::sleep_for(1ms);
	} while (test_clock::now() < until);
	return false;
}

static auto make_profile(const std::filesystem::path & folder, std::string_view name, int64_t start, std::string_view rows)
{
	const auto file = folder / name;
	std::ofstream out(file);
	out << start << '\n' << rows;
	out.close();
	require(bool(out), "Writing test profile failed");
	return file;
}

int main(int argc, char ** argv)
try
{
	const bool no_delay = argc == 2 and std::string_view(argv[1]) == "--negative-control-no-delay";
	const auto base = test_clock::now() + 1s;
	const auto folder = std::filesystem::temp_directory_path() / ("wivrn-network-impairment-test-" + std::to_string(getpid()) + "-" + std::to_string(base.time_since_epoch().count()));
	std::filesystem::create_directory(folder);
	const int receiver = open_network_socket({"127.0.0.1", "lo"});
	const int sender = open_network_socket({"127.0.0.1", "lo"});
	socket_address address;
	address.size = sizeof(address.address);
	require(getsockname(receiver, reinterpret_cast<sockaddr *>(&address.address), &address.size) == 0, "Finding loopback sink failed");
	network_impairment::route route{0, 0, 1, sender, {"127.0.0.1", "lo"}, address};
	const auto transmit = [](const auto & target, auto wire) { return send_network_packet(target.fd, target.local, target.peer, wire); };
	const auto receive = [&] {
		std::array<uint8_t, 32> bytes{};
		return recv(receiver, bytes.data(), bytes.size(), MSG_DONTWAIT);
	};
	{
		const auto start = test_clock::now();
		network_impairment link(start, {{0, 100, no_delay ? 0u : 25u, 0, 0, 0}});
		std::array<uint8_t, 4> bytes{1, 2, 3, 4};
		require(link.send(start, route, bytes, transmit) == 4, "Delayed packet must retain logical send success");
		bytes.fill(9);
		require(receive() < 0, "Timing gate: packet escaped before configured wire delay");
		link.pump(start + 24ms, transmit);
		require(receive() < 0, "Timing gate: packet escaped before 25ms");
		require(wait_for([&] {
			link.pump(test_clock::now(), transmit);
			return link.queued_bytes() == 0;
		}), "Delayed packet never released");
		std::array<uint8_t, 4> received{};
		require(recv(receiver, received.data(), received.size(), MSG_DONTWAIT) == 4 and received[0] == 1, "Queue did not own the original ciphertext");
		require(test_clock::now() - start >= 25ms, "Timing gate: no real delay measured");
	}
	if (no_delay)
		return 0;
	{
		network_impairment link(base, {{0, 20, 30, 0, 0, 0}}, folder);
		std::vector<unsigned> delivered;
		const auto sink = [&](const auto &, auto bytes) { delivered.push_back(bytes.front()); return ssize_t(bytes.size()); };
		std::array<uint8_t, 1> first{1}, second{2};
		link.send(base, route, first, sink);
		link.send(base + 21ms, route, second, sink);
		link.pump(base + 29ms, sink);
		require(delivered.empty(), "Delayed FIFO released early");
		link.pump(base + 31ms, sink);
		require(delivered == std::vector<unsigned>({1, 2}), "Window end cancelled or reordered its queued tail");
	}
	{
		network_impairment link(base, {{0, 100, 0, 0, 1000, 0}});
		std::array<uint8_t, 4> bytes{};
		unsigned physical = 0;
		const auto sink = [&](const auto &, auto data) { ++physical; return ssize_t(data.size()); };
		require(link.send(base, route, bytes, sink) == 4 and physical == 0, "Artificial loss must remain a logically successful send");
		require(link.send(base + 101ms, route, bytes, sink) == 4 and physical == 1, "Outside-window passthrough changed");
	}
	{
		network_impairment link(base, {{0, 100, 20, 0, 0, 0}}, {}, 1024);
		std::array<uint8_t, 256> bytes{};
		unsigned physical = 0;
		const auto sink = [&](const auto &, auto data) { ++physical; return ssize_t(data.size()); };
		for (unsigned i = 0; i < 100; ++i)
			require(link.send(base, route, bytes, sink) == bytes.size(), "Queue overflow suppressed logical accounting");
		require(link.queued_bytes() > 0 and link.charged_queue_bytes() <= 1024, "Queue did not enforce its memory bound");
		link.pump(base + 121ms, sink);
		require(link.queued_bytes() == 0 and physical == 0, "Packets survived maximum residence");
	}
	{
		network_impairment link(base, {{0, 100, 0, 0, 0, 1'000'000}});
		std::array<uint8_t, 1000> bytes{};
		unsigned physical = 0;
		const auto sink = [&](const auto &, auto data) { ++physical; return ssize_t(data.size()); };
		link.send(base, route, bytes, sink);
		auto other = route;
		other.path_index = 1;
		link.send(base, other, bytes, sink);
		link.pump(base + 9ms, sink);
		require(physical == 1, "Aggregate ceiling did not serialize both paths");
		link.pump(base + 18ms, sink);
		require(physical == 2, "Rate-limited packets failed to drain");
	}
	for (const auto row: {"0,1,121,0,0,0\n", "0,1,1,120,0,0\n", "0,1,0,0,1001,0\n", "0,2,0,0,0,0\n1,2,0,0,0,0\n", "0,1,0,0,0\n", "0,1,0,0,0,0,0\n", "-1,1,0,0,0,0\n"})
	{
		const auto file = make_profile(folder, "invalid.csv", base.time_since_epoch().count(), row);
		setenv("WIVRN_TEST_NETWORK_PROFILE", file.c_str(), 1);
		bool rejected = false;
		try { auto link = network_impairment::from_environment(); }
		catch (const std::exception &) { rejected = true; }
		require(rejected, "Malformed requested profile was accepted");
	}
	{
		const auto start = test_clock::now() + 300ms;
		const auto file = make_profile(folder, "integration.csv", start.time_since_epoch().count(), "0,80,0,0,1000,0\n");
		setenv("WIVRN_TEST_NETWORK_PROFILE", file.c_str(), 1);
		setenv("WIVRN_TEST_NETWORK_LOG", folder.c_str(), 1);
		const auto key = multilink::random_key();
		multilink source(true, key, {{"127.0.0.1", "lo"}}, {}, false);
		setenv("WIVRN_TEST_NETWORK_PROFILE", "/nonexistent/invalid-profile", 1);
		multilink client(false, key, {{"127.0.0.1", "lo"}}, source.endpoints(), false, false);
		require(wait_for([&] { return not source.statistics().empty() and source.statistics().front().active; }), "Loopback transport did not establish");
		require(wait_for([&] { return test_clock::now() >= start + 5ms; }), "Waiting for absolute profile start failed");
		require(test_clock::now() < start + 50ms, "Missed integration impairment window");
		const auto before = source.statistics().front().sent_bytes;
		const std::vector<uint8_t> lost_payload(200, 0x71);
		for (unsigned i = 0; i < 30; ++i)
		{
			serialization_packet packet;
			packet.serialize(lost_payload);
			require(source.send_raw(std::move(packet)) > 0, "Impairment changed application admission");
		}
		require(wait_for([&] { return source.statistics().front().sent_bytes >= before + 6000; }), "Dropped wire bypassed transport sent-byte accounting");
		require(client.receive_raw().empty(), "All-loss window delivered a data packet");
		require(wait_for([&] { return test_clock::now() >= start + 90ms; }), "Waiting for profile recovery failed");
		const std::vector<uint8_t> recovered_payload(200, 0x72);
		serialization_packet packet;
		packet.serialize(recovered_payload);
		source.send_reliable_raw(std::move(packet));
		require(wait_for([&] {
			auto incoming = client.receive_raw();
			return not incoming.empty() and incoming.deserialize<std::vector<uint8_t>>() == recovered_payload;
		}), "Transport failed to recover after an all-loss burst");
		// The acknowledgement for the recovery reports the burst missing
		require(wait_for([&] { return source.statistics().front().loss > 0; }), "Artificial loss bypassed normal ACK-loss accounting");
	}
	unsetenv("WIVRN_TEST_NETWORK_PROFILE");
	unsetenv("WIVRN_TEST_NETWORK_LOG");
	close(sender);
	close(receiver);
	std::cout << "Wire delay, ownership, FIFO tail, loss, queue bounds, aggregate ceiling, parsing and transport recovery passed\n";
	return 0;
}
catch (const std::exception & error)
{
	std::cerr << error.what() << '\n';
	return 1;
}
