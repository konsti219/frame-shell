// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
// A userspace relay keeps impairment tests unprivileged and confined to loopback.
class relay
{
public:
	struct shared_budget
	{
		std::mutex mutex;
		std::chrono::steady_clock::time_point next{};
		uint64_t rate_bps;
		explicit shared_budget(uint64_t rate) : rate_bps(rate) {}
	};

private:
	std::shared_ptr<shared_budget> budget;
	int fd;
	socket_address target;
	std::jthread worker;

public:
	std::atomic<bool> enabled = true;
	std::atomic<unsigned> drop_every = 0;
	std::atomic<unsigned> random_drop_per_mille = 0;
	std::atomic<unsigned> burst_every = 0, burst_length = 0;
	std::atomic<unsigned> corrupt_every = 0;
	std::atomic<unsigned> drop_large = 0;
	std::atomic<unsigned> drop_server_size = 0;
	std::atomic<unsigned> drop_server_count = 0;
	std::atomic<bool> drop_from_server = false;
	std::atomic<unsigned> delay_ms = 0;
	std::atomic<unsigned> jitter_ms = 0;
	std::atomic<uint64_t> rate_bps = 0;
	std::atomic<uint64_t> forwarded = 0;
	std::atomic<uint64_t> offered_wire_bytes = 0;
	std::atomic<uint64_t> dropped_rate = 0;
	std::atomic<uint64_t> dropped_impairment = 0;
	std::atomic<uint64_t> large_to_client = 0;
	network_endpoint endpoint;
	relay(network_endpoint server, std::shared_ptr<shared_budget> shared = {}, uint32_t impairment_seed = 1) : budget(std::move(shared)), fd(open_network_socket({.address = "127.0.0.1"})), target(parse_endpoint(server))
	{
		sockaddr_storage address{};
		socklen_t size = sizeof(address);
		check(getsockname(fd, reinterpret_cast<sockaddr *>(&address), &size) == 0);
		endpoint = describe_endpoint(reinterpret_cast<sockaddr *>(&address), server.interface);
		worker = std::jthread([this, impairment_seed](std::stop_token stop) {
			struct delayed
			{
				std::vector<uint8_t> data;
				socket_address destination;
				std::chrono::steady_clock::time_point due;
			};
			std::deque<delayed> queue;
			auto next_send = std::chrono::steady_clock::now();
			socket_address client;
			unsigned received = 0;
			uint32_t random_state = impairment_seed ? impairment_seed : 1;
			while (not stop.stop_requested())
			{
				const auto now = std::chrono::steady_clock::now();
				while (not queue.empty() and queue.front().due <= now)
				{
					const auto & packet = queue.front();
					if (enabled)
					{
						const auto sent = sendto(fd, packet.data.data(), packet.data.size(), 0, reinterpret_cast<const sockaddr *>(&packet.destination.address), packet.destination.size);
						if (sent > 0)
							++forwarded;
						if (sent > 500 and not(packet.destination == target))
							++large_to_client;
					}
					queue.pop_front();
				}
				pollfd event{.fd = fd, .events = POLLIN};
				if (poll(&event, 1, 1) <= 0)
					continue;
				std::array<uint8_t, 2048> bytes;
				socket_address source;
				source.size = sizeof(source.address);
				const auto count = recvfrom(fd, bytes.data(), bytes.size(), 0, reinterpret_cast<sockaddr *>(&source.address), &source.size);
				if (count <= 0)
					continue;
				offered_wire_bytes += count + 28;
				if (source == target and count == drop_server_size and drop_server_count)
				{
					--drop_server_count;
					continue;
				}
				if (source == target and drop_from_server)
					continue;
				if (count > 500 and drop_large)
				{
					--drop_large;
					continue;
				}
				++received;
				random_state ^= random_state << 13;
				random_state ^= random_state >> 17;
				random_state ^= random_state << 5;
				if (not(source == target))
					client = source;
				const auto & destination = source == target ? client : target;
				if (not enabled or not destination.size or (drop_every and received % drop_every == 0) or (random_drop_per_mille and random_state % 1000 < random_drop_per_mille) or (burst_every and received % burst_every < burst_length))
				{
					++dropped_impairment;
					continue;
				}
				if (corrupt_every and received % corrupt_every == 0)
					bytes[count - 1] ^= 0x40;
				const auto queued_at = std::chrono::steady_clock::now();
				auto scheduled = queued_at;
				if (const auto rate = rate_bps.load())
				{
					if (next_send > queued_at + 5ms)
					{
						++dropped_rate;
						continue;
					}
					scheduled = std::max(scheduled, next_send);
					next_send = scheduled + std::chrono::nanoseconds(uint64_t(count + 28) * 8'000'000'000ULL / rate);
					scheduled = next_send;
				}
				if (budget)
				{
					std::lock_guard lock(budget->mutex);
					if (budget->next > scheduled + 5ms)
					{
						++dropped_rate;
						continue;
					}
					scheduled = std::max(scheduled, budget->next);
					budget->next = scheduled + std::chrono::nanoseconds(uint64_t(count + 28) * 8'000'000'000ULL / budget->rate_bps);
					scheduled = budget->next;
				}
				const auto due = scheduled + std::chrono::milliseconds(delay_ms + (jitter_ms and received % 17 == 0 ? jitter_ms.load() : 0));
				const auto position = std::upper_bound(queue.begin(), queue.end(), due, [](const auto & time, const auto & item) { return time < item.due; });
				queue.insert(position, {std::vector<uint8_t>(bytes.begin(), bytes.begin() + count), destination, due});
			}
		});
	}
	~relay()
	{
		worker.request_stop();
		worker.join();
		close(fd);
	}
};
