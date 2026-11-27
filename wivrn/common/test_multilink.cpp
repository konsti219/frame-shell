// SPDX-License-Identifier: GPL-3.0-or-later
#include "link_probe.h"
#include "multilink.h"
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdlib>
#include <deque>
#include <iostream>
#include <mutex>
#include <poll.h>
#include <source_location>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>

using namespace wivrn;
using namespace std::chrono_literals;

static std::atomic<unsigned> observed_video_drops = 0;
static video_queue_drop observed_video_drop{};
static std::vector<uint8_t> observed_video_packet;

static void observe_video_drop(const video_queue_drop & drop)
{
	observed_video_packet.assign(drop.packet.begin(), drop.packet.end());
	observed_video_drop = drop;
	observed_video_drop.packet = observed_video_packet;
	observed_video_drops.fetch_add(1, std::memory_order_release);
}

static void check(bool value, std::source_location where = std::source_location::current())
{
	if (not value)
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

static void send(multilink & socket, const std::vector<uint8_t> & bytes, bool reliable)
{
	serialization_packet packet;
	packet.serialize(bytes);
	if (reliable)
		socket.send_reliable_raw(std::move(packet));
	else
		socket.send_raw(std::move(packet));
}

static size_t active_links(const multilink & socket)
{
	return std::ranges::count_if(socket.statistics(), [](const auto & link) { return link.active; });
}

int main(int argc, char ** argv)
{
	const bool single_link_control = argc == 2 and std::string_view(argv[1]) == "--single-link-control";
	const bool discover_path_control = argc == 2 and std::string_view(argv[1]) == "--discover-path-control";
	{
		multilink source(true, multilink::random_key(), {{"127.0.0.1", "lo"}}, {}, false, false);
		const auto send_marker = [&](uint64_t marker, auto deadline) {
			serialization_packet packet;
			packet.serialize(marker);
			check(source.send_video_raw(std::move(packet), deadline) == sizeof(marker));
		};
		send_marker(1, std::chrono::steady_clock::now() - 1ms);
		check(wait_for([&] { return source.diagnostics().expired_video == 1; }));
		check(observed_video_drops.load(std::memory_order_acquire) == 0);
		source.set_video_queue_drop_observer(observe_video_drop);
		const auto target = std::chrono::steady_clock::now() + 100ms;
		send_marker(2, target);
		check(wait_for([&] { return observed_video_drops.load(std::memory_order_acquire) == 1; }));
		check(observed_video_drop.reason == video_queue_drop_reason::deadline);
		check(observed_video_drop.target_deadline == target);
		check(observed_video_drop.deadline == target);
		check(observed_video_drop.dropped_at >= target);
		check(observed_video_drop.fragment_cursor == 0 and observed_video_drop.queued_bytes == sizeof(uint64_t));
		check(deserialization_packet({}, observed_video_packet).deserialize<uint64_t>() == 2);
		const auto vr_target = std::chrono::steady_clock::now() + 25ms;
		send_marker(3, vr_target);
		check(wait_for([&] { return observed_video_drops.load(std::memory_order_acquire) == 2; }));
		check(observed_video_drop.deadline == vr_target);
		send_marker(4, std::chrono::steady_clock::time_point{});
		check(wait_for([&] { return observed_video_drops.load(std::memory_order_acquire) == 3; }));
		check(observed_video_drop.deadline == observed_video_drop.enqueued_at + 25ms);
		source.set_video_queue_drop_observer(nullptr);
		send_marker(5, std::chrono::steady_clock::now() - 1ms);
		check(wait_for([&] { return source.diagnostics().expired_video == 5; }));
		check(observed_video_drops.load(std::memory_order_acquire) == 3);
	}
	{
		const auto key = multilink::random_key();
		const int reserved = open_network_socket({"127.0.0.1", "lo"});
		sockaddr_storage address{};
		socklen_t size = sizeof(address);
		check(getsockname(reserved, reinterpret_cast<sockaddr *>(&address), &size) == 0);
		const auto local = describe_endpoint(reinterpret_cast<const sockaddr *>(&address), "lo");
		multilink server(true, key, {{"127.0.0.30", "first"}, {"127.0.0.31", "second"}}, {local}, false);
		close(reserved);
		multilink client(false, key, {local}, {server.endpoints()[0]}, false, discover_path_control);
		check(wait_for([&] { return active_links(client) >= 1; }));
		std::this_thread::sleep_for(600ms);
		// The other server socket probes the client with the same session key.
		if (discover_path_control)
			std::cout << "Discovery control: " << client.statistics().size() << " paths\n"
			          << std::flush;
		check(client.statistics().size() == 1 and active_links(client) == 1);
		send(server, {0x31}, true);
		check(wait_for([&] {
			auto incoming = client.receive_raw();
			return not incoming.empty() and incoming.deserialize<std::vector<uint8_t>>() == std::vector<uint8_t>{0x31};
		}));
		for (unsigned cycle = 0; cycle < 2; ++cycle)
		{
			check(client.set_multilink_enabled(true));
			check(wait_for([&] { return active_links(client) >= 2; }));
			check(client.set_multilink_enabled(false));
			check(active_links(client) == 1);
			check(wait_for([&] { return active_links(server) == 1; }));
			send(server, {uint8_t(cycle)}, true);
			check(wait_for([&] {
				auto incoming = client.receive_raw();
				return not incoming.empty() and incoming.deserialize<std::vector<uint8_t>>() == std::vector<uint8_t>{uint8_t(cycle)};
			}));
		}
	}
	const auto key = multilink::random_key();
	multilink server(true, key, {{"127.0.0.1", "server-a"}, {"127.0.0.2", "server-b"}}, {}, false);
	const auto endpoints = server.endpoints();
	check(endpoints.size() == 2);
	relay first(endpoints[0]), second(endpoints[1]);
	multilink client(false, key, {{"127.0.0.3", "client"}}, {first.endpoint, second.endpoint}, false);
	check(wait_for([&] { return active_links(client) == 2 and active_links(server) == 2; }));

	// A wrong session key cannot create an authenticated path.
	{
		multilink stranger(false, multilink::random_key(), {{"127.0.0.4", "stranger"}}, {first.endpoint}, false);
		std::this_thread::sleep_for(150ms);
		check(active_links(stranger) == 0);
	}
	// The relay only serves one client, so restore the legitimate source.
	check(wait_for([&] { return active_links(client) == 2; }));
	first.drop_large = 1;
	const std::vector<uint8_t> repaired(700, 0x52);
	const auto repairs_over_second = [&] {
		uint64_t total = 0;
		for (const auto & link: client.statistics())
			if (link.remote.interface == "server-b")
				total += link.repairs_sent;
		return total;
	};
	const auto repairs_before = repairs_over_second();
	// An acknowledgement reports the loss once a later packet on that path arrives, so small
	// markers follow each attempt.
	const std::vector<uint8_t> marker(16, 0x4d);
	// Path choice depends on timing, so resend until a copy crosses the lossy relay.
	for (int attempt = 0; attempt < 10 and first.drop_large; ++attempt)
	{
		send(client, repaired, false);
		for (int i = 0; i < 4; ++i)
		{
			std::this_thread::sleep_for(2ms);
			send(client, marker, false);
		}
		check(wait_for([&] {
			auto packet = server.receive_raw();
			if (packet.empty())
				return false;
			const auto bytes = packet.deserialize<std::vector<uint8_t>>();
			if (bytes == marker)
				return false;
			check(bytes == repaired);
			return true;
		}));
	}
	check(first.drop_large == 0);
	// Repairs use another path, so this shows the dropped copy was an original sent over 'first'.
	check(repairs_over_second() > repairs_before);
	serialization_packet reply;
	reply.serialize(repaired);
	check(server.send_reply_raw(std::move(reply)) > 0);
	check(wait_for([&] {
		auto packet = client.receive_raw();
		return not packet.empty() and packet.deserialize<std::vector<uint8_t>>() == repaired;
	}));
	check(first.large_to_client == 0 and second.large_to_client == 1);
	std::this_thread::sleep_for(50ms);
	for (auto packet = server.receive_raw(); not packet.empty(); packet = server.receive_raw())
		check(packet.deserialize<std::vector<uint8_t>>() == marker);

	std::vector<uint8_t> payload(700, 0x35);
	for (int i = 0; i < 100; ++i)
	{
		send(client, payload, false);
		std::this_thread::sleep_for(1ms);
	}
	check(wait_for([&] {
		size_t count = 0;
		for (auto packet = server.receive_raw(); not packet.empty(); packet = server.receive_raw())
		{
			check(packet.deserialize<std::vector<uint8_t>>() == payload);
			++count;
		}
		return count > 0;
	}));
	const auto links = client.statistics();
	check(links.size() == 2);
	check(links[0].sent_bytes > 20'000 and links[1].sent_bytes > 20'000);

	// Reliable fragmentation must survive loss and authenticated corruption.
	first.drop_every = 7;
	second.corrupt_every = 11;
	std::vector<uint8_t> large(16'000);
	for (size_t i = 0; i < large.size(); ++i)
		large[i] = i % 251;
	send(server, large, true);
	check(wait_for([&] {
		auto packet = client.receive_raw();
		if (packet.empty())
			return false;
		check(packet.deserialize<std::vector<uint8_t>>() == large);
		return true;
	}));
	std::this_thread::sleep_for(100ms);
	check(client.receive_raw().empty());

	// Removing the original path must preserve reliable delivery and ordering.
	first.enabled = false;
	check(wait_for([&] { return active_links(client) == 1 and active_links(server) == 1; }));
	for (uint8_t i = 1; i <= 20; ++i)
		send(client, {i}, true);
	uint8_t expected = 1;
	check(wait_for([&] {
		while (true)
		{
			auto packet = server.receive_raw();
			if (packet.empty())
				break;
			const auto bytes = packet.deserialize<std::vector<uint8_t>>();
			if (bytes.size() == payload.size())
				continue;
			check(bytes == std::vector<uint8_t>{expected++});
		}
		return expected == 21;
	}));

	first.enabled = true;
	check(wait_for([&] { return active_links(client) == 2 and active_links(server) == 2; }));
	first.drop_from_server = true;
	check(wait_for([&] { return active_links(client) == 1 and active_links(server) == 1; }));
	first.drop_from_server = false;
	check(wait_for([&] { return active_links(client) == 2 and active_links(server) == 2; }));
	second.enabled = false;
	check(wait_for([&] { return active_links(client) == 1 and active_links(server) == 1; }));
	send(server, {0x71}, true);
	check(wait_for([&] {
		auto packet = client.receive_raw();
		if (packet.empty())
			return false;
		check(packet.deserialize<std::vector<uint8_t>>() == std::vector<uint8_t>{0x71});
		return true;
	}));

	{
		const auto delayed_key = multilink::random_key();
		multilink distant_server(true, delayed_key, {{"127.0.0.5", "distant"}}, {}, false);
		relay delayed(distant_server.endpoints().front());
		delayed.delay_ms = 80;
		multilink distant_client(false, delayed_key, {{"127.0.0.6", "distant-client"}}, {delayed.endpoint}, false);
		check(wait_for([&] { return active_links(distant_client) == 1 and active_links(distant_server) == 1; }));
		check(distant_client.statistics().front().rtt_ms > 100);
		send(distant_client, {0x31}, true);
		check(wait_for([&] {
			auto packet = distant_server.receive_raw();
			return not packet.empty() and packet.deserialize<std::vector<uint8_t>>() == std::vector<uint8_t>{0x31};
		}));
	}

	{
		const auto shaped_key = multilink::random_key();
		multilink shaped_server(true, shaped_key, {{"127.0.0.8", "fast"}, {"127.0.0.9", "slow"}}, {}, false);
		relay fast(shaped_server.endpoints()[0]), slow(shaped_server.endpoints()[1]);
		fast.rate_bps = 12'000'000;
		slow.rate_bps = 8'000'000;
		std::vector<network_endpoint> peers{fast.endpoint};
		if (not single_link_control)
			peers.push_back(slow.endpoint);
		multilink shaped_client(false, shaped_key, {{"127.0.0.10", "shaped-client"}}, std::move(peers), false);
		check(wait_for([&] { return active_links(shaped_client) == (single_link_control ? 1 : 2); }));
		std::vector<uint8_t> frame(1000, 0x37);
		const auto start = std::chrono::steady_clock::now();
		auto next = start;
		uint64_t received = 0;
		uint64_t fast_start = 0, slow_start = 0;
		while (std::chrono::steady_clock::now() - start < 4s)
		{
			send(shaped_client, frame, false);
			const bool measure = std::chrono::steady_clock::now() - start >= 2s;
			if (not measure)
			{
				fast_start = fast.forwarded;
				slow_start = slow.forwarded;
			}
			for (auto packet = shaped_server.receive_raw(); not packet.empty(); packet = shaped_server.receive_raw())
			{
				check(packet.deserialize<std::vector<uint8_t>>() == frame);
				if (measure)
					received += frame.size();
			}
			next += 500us;
			std::this_thread::sleep_until(next);
		}
		const double delivered_bps = received * 8 / 2.0;
		std::cout << "Constrained aggregate delivery: " << delivered_bps / 1e6 << " Mbit/s, path packet ratio " << double(fast.forwarded - fast_start) / std::max<uint64_t>(1, slow.forwarded - slow_start) << std::endl;
		for (const auto & link: shaped_client.statistics())
			std::cout << link.remote.interface << ": estimate " << link.capacity_bps / 1e6 << " Mbit/s, sent " << link.sent_bps / 1e6 << " Mbit/s, loss " << link.loss << ", RTT " << link.rtt_ms << " ms" << std::endl;
		// Above either 12/8 Mbit/s path alone, below their combined physical limit.
		check(delivered_bps > 13'000'000 and delivered_bps < 20'500'000);
		check(fast.forwarded - fast_start > (slow.forwarded - slow_start) * 1.1);
	}

	{
		const auto slow_key = multilink::random_key();
		multilink slow_server(true, slow_key, {{"127.0.0.11", "slow-control"}}, {}, false);
		relay constrained(slow_server.endpoints().front());
		constrained.rate_bps = 1'000'000;
		constrained.drop_large = 1;
		multilink slow_client(false, slow_key, {{"127.0.0.12", "slow-client"}}, {constrained.endpoint}, false);
		check(wait_for([&] { return active_links(slow_client) == 1; }));
		std::vector<uint8_t> large_control(300 * 1024, 0x67);
		const auto expected_control = large_control;
		send(slow_client, large_control, true);
		std::ranges::fill(large_control, 0x78);
		check(wait_for([&] {
			auto packet = slow_server.receive_raw();
			if (packet.empty())
				return false;
			check(packet.deserialize<std::vector<uint8_t>>() == expected_control);
			return true;
		},
		               20'000ms));
	}

	{
		const auto ipv6_key = multilink::random_key();
		multilink ipv6_server(true, ipv6_key, {{"::1", "lo"}}, {}, false);
		multilink ipv6_client(false, ipv6_key, {{"::1", "lo"}}, ipv6_server.endpoints(), false);
		check(wait_for([&] { return active_links(ipv6_client) == 1 and active_links(ipv6_server) == 1; }));
		send(ipv6_client, {0x36}, true);
		check(wait_for([&] {
			auto packet = ipv6_server.receive_raw();
			return not packet.empty() and packet.deserialize<std::vector<uint8_t>>() == std::vector<uint8_t>{0x36};
		}));
		const network_endpoint scoped{"fe80::1234", "test-interface", 9757, 17};
		const auto address = parse_endpoint(scoped);
		check(describe_endpoint(reinterpret_cast<const sockaddr *>(&address.address), scoped.interface) == scoped);
	}

	{
		const auto credit_key = multilink::random_key();
		multilink credit_server(true, credit_key, {{"127.0.0.13", "a"}}, {}, false);
		relay credit_relay(credit_server.endpoints().front());
		multilink credit_client(false, credit_key, {{"127.0.0.14", "b"}}, {credit_relay.endpoint}, false);
		check(wait_for([&] { return active_links(credit_client) == 1; }));
		credit_relay.drop_server_size = 49;
		credit_relay.drop_server_count = 1;
		send(credit_client, {1}, true);
		check(wait_for([&] { return credit_relay.drop_server_count == 0; }));
		send(credit_client, {2}, true);
		check(wait_for([&] { return credit_client.diagnostics().reliable_messages == 0; }));
		check(credit_client.diagnostics().queued_bytes == 0);
		check(credit_server.receive_raw().deserialize<std::vector<uint8_t>>() == std::vector<uint8_t>{1});
		check(credit_server.receive_raw().deserialize<std::vector<uint8_t>>() == std::vector<uint8_t>{2});

		const std::vector<uint8_t> icon(1024 * 1024, 0x53);
		unsigned accepted = 0;
		for (unsigned i = 0; i < 20; ++i)
		{
			serialization_packet packet;
			packet.serialize(icon);
			accepted += credit_client.send_bulk_raw(std::move(packet)) > 0;
		}
		check(accepted > 0 and accepted < 20);
		send(credit_client, {3}, true);
		unsigned bulk_received = 0;
		bool command_received = false;
		check(wait_for([&] {
			for (auto incoming = credit_server.receive_raw(); not incoming.empty(); incoming = credit_server.receive_raw())
			{
				const auto bytes = incoming.deserialize<std::vector<uint8_t>>();
				if (bytes == std::vector<uint8_t>{3})
					command_received = true;
				else
				{
					check(bytes == icon);
					++bulk_received;
				}
			}
			return command_received and bulk_received == accepted and credit_client.diagnostics().queued_bytes == 0;
		},
		               10s));
		check(credit_client.diagnostics().rejected_bulk > 0);
		const std::vector<uint8_t> keyframe(1400, 0x56);
		for (unsigned i = 0; i < 1500; ++i)
		{
			serialization_packet part;
			part.serialize(keyframe);
			check(credit_client.send_video_raw(std::move(part), std::chrono::steady_clock::now() + 25ms, true) > 0);
		}
		send(credit_client, {4}, true);
		unsigned protected_received = 0;
		command_received = false;
		check(wait_for([&] {
			for (auto incoming = credit_server.receive_raw(); not incoming.empty(); incoming = credit_server.receive_raw())
			{
				const auto bytes = incoming.deserialize<std::vector<uint8_t>>();
				if (bytes == std::vector<uint8_t>{4})
				{
					check(protected_received < 1500);
					command_received = true;
				}
				else
				{
					check(bytes == keyframe);
					++protected_received;
				}
			}
			return command_received and protected_received == 1500 and credit_client.diagnostics().queued_bytes == 0;
		},
		               10s));
	}

	// The responder must timestamp arrival, rather than the browser's next poll.
	int probe_fd = open_network_socket({.address = "127.0.0.1"});
	sockaddr_in probe_address{};
	socklen_t probe_size = sizeof(probe_address);
	check(getsockname(probe_fd, reinterpret_cast<sockaddr *>(&probe_address), &probe_size) == 0);
	const auto probe_endpoint = describe_endpoint(reinterpret_cast<sockaddr *>(&probe_address));
	close(probe_fd);
	{
		link_probe_responder responder(probe_endpoint.port);
		link_probe probe;
		std::this_thread::sleep_for(30ms);
		check(not probe.sample({"127.0.0.1", "lo"}, probe_endpoint));
		std::this_thread::sleep_for(150ms);
		const auto latency = probe.sample({"127.0.0.1", "lo"}, probe_endpoint);
		check(latency and *latency < 50);
	}

	server.prepare_fork();
	const auto child = fork();
	check(child >= 0);
	if (child == 0)
	{
		server.resume_after_fork();
		send(server, {0x61}, true);
		const bool answered = wait_for([&] {
			auto packet = server.receive_raw();
			return not packet.empty() and packet.deserialize<std::vector<uint8_t>>() == std::vector<uint8_t>{0x62};
		});
		_exit(answered ? 0 : 1);
	}
	check(wait_for([&] {
		auto packet = client.receive_raw();
		if (packet.empty())
			return false;
		check(packet.deserialize<std::vector<uint8_t>>() == std::vector<uint8_t>{0x61});
		return true;
	}));
	send(client, {0x62}, true);
	int result = 0;
	check(waitpid(child, &result, 0) == child and WIFEXITED(result) and WEXITSTATUS(result) == 0);

	std::cout << "Concurrent paths, fragmentation, authentication, loss, ordered control and either-path failover passed\n";
}
