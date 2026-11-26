// SPDX-License-Identifier: GPL-3.0-or-later
#include "multilink.h"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cstdlib>
#include <deque>
#include <iostream>
#include <memory>
#include <mutex>
#include <poll.h>
#include <source_location>
#include <thread>
#include <unistd.h>

using namespace wivrn;
using namespace std::chrono_literals;

static void check(bool condition, std::source_location where = std::source_location::current())
{
	if (not condition)
	{
		std::cerr << where.file_name() << ':' << where.line() << '\n';
		std::abort();
	}
}

template <typename Predicate>
static bool wait_for(Predicate predicate, std::chrono::milliseconds timeout = 3000ms)
{
	const auto until = std::chrono::steady_clock::now() + timeout;
	do
	{
		if (predicate())
			return true;
		std::this_thread::sleep_for(1ms);
	} while (std::chrono::steady_clock::now() < until);
	return false;
}

#include "test_network_relay.h"

struct marker_observation
{
	size_t received = 0;
	double max_latency_ms = 0;
	bool saw_stale_high_rtt = false;
	uint64_t min_capacity_bps = UINT64_MAX;
};

static marker_observation send_markers(multilink & server, multilink & client, double baseline_rtt_ms)
{
	constexpr size_t count = 200;
	std::array<bool, count> received{};
	std::array<std::chrono::steady_clock::time_point, count> sent_at{};
	marker_observation result;
	const auto start = std::chrono::steady_clock::now();
	const auto drain = [&] {
		for (auto packet = client.receive_raw(); not packet.empty(); packet = client.receive_raw())
		{
			const auto bytes = packet.deserialize<std::vector<uint8_t>>();
			check(bytes.size() == 800 and bytes.front() < count);
			check(std::ranges::all_of(bytes.begin() + 1, bytes.end(), [](uint8_t value) { return value == 0x35; }));
			const auto id = bytes.front();
			check(not received[id]);
			received[id] = true;
			++result.received;
			result.max_latency_ms = std::max(result.max_latency_ms,
			        std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - sent_at[id]).count());
		}
		const auto stats = server.statistics();
		if (not stats.empty())
		{
			result.saw_stale_high_rtt |= stats.front().rtt_ms > baseline_rtt_ms + 5;
			result.min_capacity_bps = std::min(result.min_capacity_bps, stats.front().capacity_bps);
		}
	};
	for (uint8_t i = 0; i < count; ++i)
	{
		std::vector<uint8_t> bytes(800, 0x35);
		bytes.front() = i;
		serialization_packet packet;
		packet.serialize(bytes);
		sent_at[i] = std::chrono::steady_clock::now();
		if (i % 2 == 0)
			server.send_video_raw(std::move(packet), sent_at[i] + 100ms);
		else
			server.send_raw(std::move(packet));
		std::this_thread::sleep_until(start + (i + 1) * 2ms);
		drain();
	}
	const auto deadline = std::chrono::steady_clock::now() + 500ms;
	while (result.received < count and std::chrono::steady_clock::now() < deadline)
	{
		drain();
		std::this_thread::sleep_for(1ms);
	}
	drain();
	return result;
}

static void stale_rtt_recovery_keeps_capacity_and_delivery()
{
	const auto key = multilink::random_key();
	multilink server(true, key, {{"127.0.0.1", "server"}}, {}, false);
	relay link(server.endpoints().front());
	multilink client(false, key, {{"127.0.0.2", "client"}}, {link.endpoint}, false, false);
	check(wait_for([&] {
		const auto stats = server.statistics();
		return stats.size() == 1 and stats.front().active and stats.front().rtt_ms < 5;
	}));
	std::this_thread::sleep_for(250ms);
	const auto baseline = server.statistics().front();
	const auto capacity_before = baseline.capacity_bps;
	link.delay_ms = 40;
	check(wait_for([&] { return server.statistics().front().rtt_ms > baseline.rtt_ms + 30; }, 900ms));
	link.delay_ms = 0;
	std::this_thread::sleep_for(150ms); // let delayed probes drain while the EWMA is still elevated
	const auto recovered_probe = server.statistics().front();
	check(recovered_probe.rtt_ms > baseline.rtt_ms + 5);
	check(recovered_probe.capacity_bps == capacity_before);

	const auto result = send_markers(server, client, baseline.rtt_ms);
	std::cout << "Stale-RTT recovery: delivered " << result.received << "/200, max " << result.max_latency_ms
	          << " ms, min capacity " << result.min_capacity_bps / 1e6 << " Mbit/s\n" << std::flush;
	check(result.received == 200);
	check(result.max_latency_ms < 50);
	check(result.saw_stale_high_rtt);
	check(result.min_capacity_bps >= capacity_before * 95 / 100);
}

static void sustained_rtt_inflation_still_reduces_capacity()
{
	const auto key = multilink::random_key();
	multilink server(true, key, {{"127.0.0.1", "server"}}, {}, false);
	relay link(server.endpoints().front());
	multilink client(false, key, {{"127.0.0.2", "client"}}, {link.endpoint}, false, false);
	check(wait_for([&] {
		const auto stats = server.statistics();
		return stats.size() == 1 and stats.front().active and stats.front().rtt_ms < 5;
	}));
	std::this_thread::sleep_for(250ms);
	const auto baseline = server.statistics().front();
	const auto capacity_before = baseline.capacity_bps;
	link.delay_ms = 40;
	check(wait_for([&] { return server.statistics().front().rtt_ms > baseline.rtt_ms + 30; }, 900ms));
	const auto result = send_markers(server, client, baseline.rtt_ms);
	std::this_thread::sleep_for(200ms); // allow the last delayed ACKs into a controller sample
	const auto after = server.statistics().front();
	std::cout << "Sustained-RTT control: delivered " << result.received << "/200, capacity "
	          << capacity_before / 1e6 << " -> " << after.capacity_bps / 1e6 << " Mbit/s\n" << std::flush;
	check(result.received == 200);
	check(result.max_latency_ms < 100);
	check(after.capacity_bps <= capacity_before * 90 / 100);
}

int main()
{
	const auto key = multilink::random_key();
	multilink server(true, key, {{"127.0.0.1", "server"}}, {}, false);
	relay link(server.endpoints().front());
	multilink client(false, key, {{"127.0.0.2", "client"}}, {link.endpoint}, false, false);
	check(wait_for([&] {
		const auto stats = server.statistics();
		return stats.size() == 1 and stats.front().active and stats.front().rtt_ms < 5;
	}));
	std::this_thread::sleep_for(300ms);

	// Delay a few probes, then restore a fast path before sending fresh video.
	link.delay_ms = 40;
	check(wait_for([&] { return server.statistics().front().jitter_ms > 10; }));
	link.delay_ms = 0;
	std::this_thread::sleep_for(100ms);
	const auto stats = server.statistics().front();
	check(stats.active and stats.rtt_ms / 2 + 2 * stats.jitter_ms + 1 > 25);

	std::array<bool, 60> received{};
	const auto drain = [&] {
		for (auto packet = client.receive_raw(); not packet.empty(); packet = client.receive_raw())
		{
			const auto bytes = packet.deserialize<std::vector<uint8_t>>();
			check(bytes.size() == 800 and bytes.front() < received.size());
			check(std::ranges::all_of(bytes.begin() + 1, bytes.end(), [](uint8_t value) { return value == 0x35; }));
			received[bytes.front()] = true;
		}
	};
	for (uint8_t i = 0; i < received.size() / 2; ++i)
	{
		std::vector<uint8_t> bytes(800, 0x35);
		bytes.front() = i;
		serialization_packet packet;
		packet.serialize(bytes);
		server.send_video_raw(std::move(packet), std::chrono::steady_clock::now() + 10ms);
		bytes.front() += received.size() / 2;
		serialization_packet tracking;
		tracking.serialize(bytes);
		server.send_raw(std::move(tracking));
		std::this_thread::sleep_for(2ms);
		drain();
	}
	std::this_thread::sleep_for(50ms);
	drain();
	const auto video = std::count(received.begin(), received.begin() + received.size() / 2, true);
	const auto tracking = std::count(received.begin() + received.size() / 2, received.end(), true);
	std::cout << "Fresh packets after jitter spike: video " << video << "/30, tracking " << tracking << "/30\n"
	          << std::flush;
	check(video >= 27 and tracking >= 27);

	const auto expired = server.diagnostics().expired_video;
	serialization_packet late;
	late.serialize(std::vector<uint8_t>(800, 0xff));
	server.send_video_raw(std::move(late), std::chrono::steady_clock::now() - 1ms);
	check(wait_for([&] { return server.diagnostics().expired_video > expired; }));
	check(client.receive_raw().empty());

	stale_rtt_recovery_keeps_capacity_and_delivery();
	sustained_rtt_inflation_still_reduces_capacity();
}
