// SPDX-License-Identifier: GPL-3.0-or-later
#include "multilink.h"
#include "network_impairment.h"
#include "xor_fec.h"

#include <algorithm>
#include <bitset>
#include <chrono>
#include <cmath>
#include <cstring>
#include <deque>
#include <exception>
#include <limits>
#include <map>
#include <mutex>
#include <net/if.h>
#include <openssl/evp.h>
#include <openssl/rand.h>
#include <poll.h>
#include <ranges>
#include <sys/eventfd.h>
#include <thread>
#include <unistd.h>

namespace wivrn
{
namespace
{
using clock = std::chrono::steady_clock;
using namespace std::chrono_literals;

constexpr size_t fragment_size = 1100;
constexpr size_t max_message = 4 * 1024 * 1024;
constexpr size_t memory_limit = 16 * 1024 * 1024;
constexpr size_t max_paths = 32 * 32;
constexpr size_t receive_window = 4096;
// Each acknowledgement reports this many recent packets, so a lost or late one is covered by the next
constexpr size_t ack_window = 1024;
// A pause this long in arrivals from the peer means its packets, acknowledgements included, were held
constexpr auto return_gap = std::chrono::milliseconds(8);

constexpr uint64_t bulk_bit = uint64_t(1) << 63;
constexpr uint64_t video_bit = uint64_t(1) << 62;
constexpr uint64_t channel_bits = bulk_bit | video_bit;

static size_t channel_index(uint64_t id)
{
	return id & bulk_bit ? 1 : id & video_bit ? 2
	                                          : 0;
}

constexpr std::array<uint8_t, 4> magic{'W', 'Y', 'M', 1};

enum class kind : uint8_t
{
	ping,
	pong,
	data,
	reliable,
	ack,
	delivered,
	endpoint,
	check_delivery,
	retry,
	bulk,
	video,
	parity,
	reliable_video
};

// Packets that carry message fragments and are acknowledged
bool carries_data(kind type)
{
	return type == kind::data or type == kind::video or type == kind::reliable or type == kind::bulk or type == kind::reliable_video;
}

bool reliable_kind(kind type)
{
	return type == kind::reliable or type == kind::bulk or type == kind::reliable_video;
}

std::array<uint8_t, 12> make_nonce(bool from_server, uint64_t counter)
{
	std::array<uint8_t, 12> nonce{};
	nonce[0] = from_server ? 1 : 0;
	for (size_t i = 0; i < 8; ++i)
		nonce[i + 4] = counter >> (i * 8);
	return nonce;
}

int64_t nanoseconds(clock::time_point time)
{
	return std::chrono::duration_cast<std::chrono::nanoseconds>(time.time_since_epoch()).count();
}

void append(std::vector<uint8_t> & data, uint64_t value, size_t bytes)
{
	for (size_t i = 0; i < bytes; ++i)
		data.push_back(value >> (i * 8));
}

uint64_t read(std::span<const uint8_t> & data, size_t bytes)
{
	if (data.size() < bytes)
		throw std::runtime_error("Truncated multilink packet");

	uint64_t result = 0;
	for (size_t i = 0; i < bytes; ++i)
		result |= uint64_t(data[i]) << (8 * i);
	data = data.subspan(bytes);
	return result;
}

std::vector<uint8_t> flatten(serialization_packet & packet)
{
	std::vector<uint8_t> result;
	for (const auto span: static_cast<std::vector<std::span<uint8_t>> &>(packet))
	{
		if (span.size() > max_message - result.size())
			throw std::runtime_error("Multilink message is too large");
		result.insert(result.end(), span.begin(), span.end());
	}
	return result;
}

deserialization_packet owned_packet(std::span<const uint8_t> bytes)
{
	auto memory = std::make_shared<uint8_t[]>(bytes.size());
	std::memcpy(memory.get(), bytes.data(), bytes.size());
	return deserialization_packet(memory, {memory.get(), bytes.size()});
}

struct sequence_window
{
	uint64_t highest = 0;
	std::bitset<receive_window> seen;

	bool contains(uint64_t value) const
	{
		return value <= highest and (highest - value >= receive_window or seen[highest - value]);
	}

	void insert(uint64_t value)
	{
		if (value > highest)
		{
			const auto shift = value - highest;
			if (shift >= receive_window)
				seen.reset();
			else
				seen <<= shift;
			highest = value;
		}
		if (highest - value < receive_window)
			seen.set(highest - value);
	}
};
} // namespace

struct multilink::implementation
{
	struct socket
	{
		int fd;
		uint64_t identity = 0;
		network_endpoint local;

		socket(int fd, network_endpoint local) : fd(fd), local(std::move(local)) {}
		socket(const socket &) = delete;
		~socket()
		{
			if (fd >= 0)
				close(fd);
		}
	};

	struct path
	{
		size_t socket_index;
		socket_address peer;
		link_statistics stats;
		bool validated = false;
		clock::time_point seen{};
		clock::time_point confirmed{};
		clock::time_point created = clock::now();
		clock::time_point probe{};
		std::map<uint64_t, clock::time_point> probes;

		clock::time_point next_send{};
		clock::time_point next_control{};
		double finish = 0;
		double ack_age_max_ms = 0;
		// Arrivals from the peer resumed after a pause at this time: acknowledgements for packets sent
		// before it may still be held on the way back
		clock::time_point resumed{};

		// One-way delay to the peer, offset by the unknown clock difference, and its lowest value of the
		// last 10 s, from the arrival times the peer reports
		std::deque<std::pair<clock::time_point, int64_t>> delay_window;
		double queue_delay_ms = 0;
		double latest_queue_delay_ms = 0;
		clock::time_point delay_sampled{};
		// Delivered rate when the current run of delay cuts began; they do not go below it, so a
		// lasting step in the base delay costs a tenth of the rate at most
		uint64_t delay_floor_bps = 0;
		// Estimate before the last congestion cut: growth slows near it
		uint64_t congested_bps = 0;
		double repair_credit = 0;
		clock::time_point repair_refill = clock::now();
		// Lowest RTT of the last 10 s, as a congestion baseline that follows lasting route changes
		std::deque<std::pair<clock::time_point, double>> rtt_window;
		// Grow quickly until the first congestion signal, like TCP slow start
		bool slow_start = true;
		// The last controller interval carried enough traffic to judge the estimate
		bool loaded = false;

		clock::time_point sample_start = clock::now();
		uint64_t acknowledged = 0;
		uint64_t lost = 0;
		uint64_t sent = 0;
		uint64_t previous_sent = 0;
		uint64_t previous_received = 0;

		uint64_t video_counter = 0;
		clock::time_point latest_acked_send{};
		sequence_window received_video;
		std::vector<fec_packet> fec_out;
		clock::time_point fec_deadline{};

		// Acknowledgements still to send, and the newest acknowledged packet with its arrival time
		uint8_t ack_repeats = 0;
		uint64_t ack_sample = 0;
		clock::time_point ack_sample_arrival{};
	};

	struct message
	{
		uint64_t id;
		std::vector<uint8_t> data;
		clock::time_point created;
		clock::time_point sent{};
		bool reliable;
		bool bulk = false;
		bool video = false;
		bool protect = false;
		bool reliable_video = false;
		uint64_t control_dependency = 0;
		size_t cursor = 0;
		clock::time_point deadline{};
		clock::time_point target_deadline{};
		std::vector<bool> acknowledged_fragments;
	};

	struct assembly
	{
		clock::time_point created;
		std::vector<std::vector<uint8_t>> fragments;
		size_t received = 0;
		size_t bytes = 0;
	};

	struct transmission
	{
		size_t path_index;
		clock::time_point time;
		size_t bytes;
		uint64_t message = 0;
		uint16_t fragment = 0, count = 0;
		std::vector<uint8_t> payload;
		clock::time_point deadline{};
		bool reliable = false;
		bool video = false;
		// An acknowledgement reported it missing while acknowledging a later packet on its path
		bool missing = false;
	};

	// Configuration
	mutable std::mutex mutex;
	bool server;
	key_type key;
	bool refresh;
	bool discover;
	std::optional<size_t> single_path;
	uint16_t port;
	bool include_loopback;
	uint64_t generation = network_generation();

	// Sockets and paths
	std::vector<std::unique_ptr<socket>> sockets;
	uint64_t next_socket_identity = 0;
	std::unique_ptr<network_impairment> impairment;
	std::vector<network_endpoint> remote_endpoints;
	std::vector<clock::time_point> remote_seen;
	std::vector<path> paths;

	// Message queues
	std::deque<message> pending;
	std::deque<message> video;
	std::map<uint64_t, message> reliable;
	std::map<std::pair<bool, uint64_t>, assembly> assemblies;
	std::map<uint64_t, std::vector<uint8_t>> ordered;
	std::deque<std::pair<std::vector<uint8_t>, size_t>> received;
	std::map<std::thread::id, size_t> receive_paths;

	// Sent and received packet numbers
	std::map<uint64_t, transmission> transmissions;
	// Send times by packet number, for delay samples whose transmission an earlier acknowledgement removed
	struct send_record
	{
		uint64_t nonce = 0;
		size_t path_index = 0;
		clock::time_point time{};
	};
	std::array<send_record, 4096> send_times{};
	sequence_window replay, delivered;
	// Data accepted from a validated path or rebuilt from parity: what acknowledgements report
	sequence_window accepted;

	// Reported while no path is usable
	uint64_t last_capacity_bps = 0;

	// Packet and message identifiers
	uint64_t counter = 0;
	uint64_t message_id = 0;
	uint64_t reliable_id = 0;
	uint64_t expected_reliable = 1;
	uint64_t expected_bulk = bulk_bit | 1;
	uint64_t bulk_id = bulk_bit;
	uint64_t video_id = video_bit;
	uint64_t expected_video = video_bit | 1;
	std::array<uint64_t, 3> peer_expected{1, bulk_bit | 1, video_bit | 1};

	// Options, diagnostics and per-channel queue bytes
	size_t reliable_video_bytes = 0;
	multilink_options options;
	multilink_diagnostics counters;
	video_queue_drop_observer video_drop_observer = nullptr;
	size_t bulk_bytes = 0;

	// FEC repair
	std::map<uint64_t, std::vector<uint8_t>> fec_received;
	std::deque<std::pair<clock::time_point, uint64_t>> fec_expiry;
	struct pending_parity
	{
		clock::time_point created;
		size_t path;
		std::vector<uint8_t> data;
	};
	std::deque<pending_parity> fec_pending;

	// Shared bottleneck
	clock::time_point aggregate_next_send{};
	clock::time_point aggregate_sample = clock::now();
	uint64_t coupled_capacity_bps = 0;
	uint64_t group_acked = 0, group_sent = 0, group_lost = 0;

	// Memory accounting
	size_t queued_bytes = 0;
	size_t receive_bytes = 0;
	size_t assembly_bytes = 0;
	size_t ordered_bytes = 0;

	// Notification and errors
	int ready = -1;
	int wake = -1;
	std::exception_ptr error;

	// Timers
	clock::time_point started = clock::now();
	clock::time_point last_received = started;
	clock::time_point last_refresh{};
	clock::time_point last_advertisement{};
	clock::time_point last_ack{};

	std::unique_ptr<EVP_CIPHER_CTX, decltype(&EVP_CIPHER_CTX_free)> encrypt{EVP_CIPHER_CTX_new(), EVP_CIPHER_CTX_free};
	std::unique_ptr<EVP_CIPHER_CTX, decltype(&EVP_CIPHER_CTX_free)> decrypt{EVP_CIPHER_CTX_new(), EVP_CIPHER_CTX_free};
	std::jthread worker;

	// Lifetime and worker thread

	implementation(bool server, key_type key, std::vector<network_endpoint> local, std::vector<network_endpoint> remote, bool refresh, bool discover) :
	        server(server),
	        key(key),
	        refresh(refresh),
	        discover(discover),
	        port(local.empty() ? 0 : local.front().port),
	        include_loopback(false),
	        remote_endpoints(std::move(remote))
	{
		if (local.empty() or local.size() > 32 or remote_endpoints.size() > 32)
			throw std::invalid_argument("Invalid multilink endpoints");
		if (not encrypt or not decrypt)
			throw std::runtime_error("Cannot allocate multilink encryption context");
		if (server)
			impairment = network_impairment::from_environment();

		try
		{
			ready = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
			wake = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
			if (ready < 0 or wake < 0)
				throw std::system_error(errno, std::generic_category(), "Creating multilink notification");

			for (const auto & endpoint: local)
			{
				include_loopback |= endpoint.interface == "lo" or endpoint.address.starts_with("127.") or endpoint.address == "::1";
				add_socket(endpoint);
			}
			if (sockets.empty())
				throw std::runtime_error("Cannot bind any multilink interfaces");

			add_candidates();
			remote_seen.resize(remote_endpoints.size(), clock::now());
			worker = std::jthread([this](std::stop_token stop) { run(stop); });
		}
		catch (...)
		{
			if (ready >= 0)
				close(ready);
			if (wake >= 0)
				close(wake);
			throw;
		}
	}

	~implementation()
	{
		worker.request_stop();
		signal(wake);
		if (worker.joinable())
			worker.join();
		close(ready);
		close(wake);
		OPENSSL_cleanse(key.data(), key.size());
	}

	static void signal(int fd)
	{
		uint64_t value = 1;
		const auto result = ::write(fd, &value, sizeof(value));
		(void)result;
	}

	static void drain(int fd)
	{
		uint64_t value;
		const auto result = ::read(fd, &value, sizeof(value));
		(void)result;
	}

	void run(std::stop_token stop)
	{
		try
		{
			while (not stop.stop_requested())
			{
				std::vector<pollfd> fds{{.fd = wake, .events = POLLIN}};
				{
					std::lock_guard lock(mutex);
					tick(clock::now());
					for (const auto & s: sockets)
						fds.push_back({.fd = s->fd, .events = POLLIN});
				}

				if (::poll(fds.data(), fds.size(), 1) < 0 and errno != EINTR)
					throw std::system_error(errno, std::generic_category(), "Polling multilink sockets");

				std::lock_guard lock(mutex);
				if (fds[0].revents & POLLIN)
					drain(wake);
				for (size_t i = 1; i < fds.size(); ++i)
					if (fds[i].revents & POLLIN)
						receive(i - 1);
			}
		}
		catch (...)
		{
			std::lock_guard lock(mutex);
			error = std::current_exception();
			signal(ready);
		}
	}

	// Sockets, paths and probing

	void add_socket(network_endpoint local)
	{
		try
		{
			const int fd = open_network_socket(local);
			auto item = std::make_unique<socket>(fd, local);
			item->identity = ++next_socket_identity;

			sockaddr_storage address{};
			socklen_t size = sizeof(address);
			if (getsockname(fd, reinterpret_cast<sockaddr *>(&address), &size) != 0)
				throw std::system_error(errno, std::generic_category());
			item->local = describe_endpoint(reinterpret_cast<sockaddr *>(&address), local.interface);

			if (auto stale = std::ranges::find_if(sockets, [](const auto & s) { return s->fd < 0; }); stale != sockets.end())
			{
				const size_t index = stale - sockets.begin();
				std::erase_if(transmissions, [&](const auto & entry) {
					return paths[entry.second.path_index].socket_index == index;
				});
				for (auto & p: paths)
					if (p.socket_index == index)
					{
						path replacement{.socket_index = index, .peer = p.peer};
						replacement.stats.local = item->local;
						replacement.stats.remote = p.stats.remote;
						replacement.stats.capacity_bps = 20'000'000;
						p = std::move(replacement);
					}
				*stale = std::move(item);
			}
			else
				sockets.push_back(std::move(item));
		}
		catch (const std::system_error &)
		{
			// Unavailable interfaces are retried on the next refresh.
		}
	}

	void add_candidates()
	{
		for (auto endpoint: remote_endpoints)
		{
			for (size_t i = 0; i < sockets.size(); ++i)
			{
				if (sockets[i]->fd < 0)
					continue;

				const auto source = parse_endpoint(sockets[i]->local);
				auto target = parse_endpoint(endpoint);
				if (source.address.ss_family != target.address.ss_family)
					continue;

				if (target.address.ss_family == AF_INET6)
				{
					auto & v6 = reinterpret_cast<sockaddr_in6 &>(target.address);
					if (IN6_IS_ADDR_LINKLOCAL(&v6.sin6_addr))
						v6.sin6_scope_id = if_nametoindex(sockets[i]->local.interface.c_str());
				}
				find_path(i, target, endpoint.interface);
			}
		}
	}

	size_t find_path(size_t socket_index, const socket_address & peer, std::string interface = {})
	{
		for (size_t i = 0; i < paths.size(); ++i)
			if (paths[i].socket_index == socket_index and paths[i].peer == peer)
				return i;

		path p{.socket_index = socket_index, .peer = peer};
		p.stats.local = sockets[socket_index]->local;
		p.stats.remote = describe_endpoint(reinterpret_cast<const sockaddr *>(&peer.address), std::move(interface));
		p.stats.capacity_bps = 20'000'000;

		if (paths.size() >= max_paths)
		{
			const auto now = clock::now();
			auto stale = std::ranges::find_if(paths, [&](const auto & existing) {
				return sockets[existing.socket_index]->fd < 0 or
				       (not active(existing, now) and now - std::max(existing.seen, existing.created) > 2s);
			});
			if (stale == paths.end())
				return max_paths;

			const size_t index = stale - paths.begin();
			std::erase_if(transmissions, [index](const auto & entry) { return entry.second.path_index == index; });
			*stale = std::move(p);
			return index;
		}
		paths.push_back(std::move(p));
		return paths.size() - 1;
	}

	bool active(const path & p, clock::time_point now) const
	{
		const bool selected = not single_path or &p == &paths[*single_path];
		return selected and sockets[p.socket_index]->fd >= 0 and p.validated and now - p.confirmed < 500ms;
	}

	std::vector<size_t> usable(clock::time_point now) const
	{
		std::map<std::pair<std::string, std::string>, size_t> groups;
		for (size_t i = 0; i < paths.size(); ++i)
		{
			const auto & p = paths[i];
			if (not active(p, now))
				continue;

			auto group = std::make_pair(p.stats.local.interface, p.stats.remote.interface);
			auto [it, inserted] = groups.emplace(group, i);
			if (not inserted and p.stats.rtt_ms < paths[it->second].stats.rtt_ms)
				it->second = i;
		}

		std::vector<size_t> result;
		for (const auto & [_, index]: groups)
			result.push_back(index);
		return result;
	}

	size_t preferred_single_path(const std::vector<size_t> & candidates) const
	{
		return *std::ranges::min_element(candidates, {}, [&](size_t i) {
			return arrival_delay_ms(paths[i]) + 50 * paths[i].stats.loss;
		});
	}

	// Encryption and framing

	bool stale_route(const network_impairment::route & destination) const
	{
		if (destination.socket_index >= sockets.size() or destination.path_index >= paths.size())
			return true;

		const auto & s = *sockets[destination.socket_index];
		const auto & p = paths[destination.path_index];
		return s.fd != destination.fd or
		       s.identity != destination.socket_identity or
		       s.local != destination.local or
		       p.socket_index != destination.socket_index or
		       not(p.peer == destination.peer);
	}

	ssize_t send_impaired_wire(const network_impairment::route & destination, std::span<const uint8_t> wire)
	{
		if (stale_route(destination))
		{
			errno = ESTALE;
			return -1;
		}
		return send_network_packet(destination.fd, destination.local, destination.peer, wire);
	}

	bool transmit(size_t index,
	              kind type,
	              uint64_t id,
	              uint16_t fragment,
	              uint16_t count,
	              std::span<const uint8_t> payload = {},
	              clock::time_point deadline = {})
	{
		if (index >= paths.size() or payload.size() > (type == kind::parity ? 1200 : fragment_size))
			return false;

		auto & p = paths[index];
		const int fd = sockets[p.socket_index]->fd;
		if (fd < 0)
			return false;

		if (counter == std::numeric_limits<uint64_t>::max())
			throw std::runtime_error("Multilink nonce exhausted");
		const uint64_t nonce_counter = ++counter;

		std::vector<uint8_t> plain{uint8_t(type)};
		append(plain, id, 8);
		append(plain, fragment, 2);
		append(plain, count, 2);
		if (type == kind::video or type == kind::reliable_video)
			append(plain, ++p.video_counter, 8);
		plain.insert(plain.end(), payload.begin(), payload.end());

		const auto nonce = make_nonce(server, nonce_counter);
		std::vector<uint8_t> wire(magic.begin(), magic.end());
		append(wire, nonce_counter, 8);
		wire.resize(12 + plain.size() + 16);
		int length = 0, final = 0;
		if (EVP_EncryptInit_ex(encrypt.get(), EVP_aes_256_gcm(), nullptr, key.data(), nonce.data()) != 1 or
		    EVP_EncryptUpdate(encrypt.get(), nullptr, &length, wire.data(), 12) != 1 or
		    EVP_EncryptUpdate(encrypt.get(), wire.data() + 12, &length, plain.data(), plain.size()) != 1 or
		    EVP_EncryptFinal_ex(encrypt.get(), wire.data() + 12 + length, &final) != 1 or
		    EVP_CIPHER_CTX_ctrl(encrypt.get(), EVP_CTRL_GCM_GET_TAG, 16, wire.data() + wire.size() - 16) != 1)
			throw std::runtime_error("Multilink encryption failed");

		// Before sending: a stall in the call can then only raise a delay sample, which the minimum ignores
		const auto sent_time = clock::now();
		const auto result = impairment
		                            ? impairment->send(sent_time,
		                                               {index, p.socket_index, sockets[p.socket_index]->identity, fd, p.stats.local, p.peer},
		                                               wire,
		                                               [this](const auto & destination, auto bytes) { return send_impaired_wire(destination, bytes); })
		                            : send_network_packet(fd, p.stats.local, p.peer, wire);
		if (result < 0)
		{
			if (errno != EAGAIN and errno != EWOULDBLOCK and errno != ENOBUFS)
				p.validated = false;
			return false;
		}

		p.stats.sent_bytes += result;
		const auto aggregate_limit = options.aggregate_limit_bps ? options.aggregate_limit_bps : coupled_capacity_bps;
		if (aggregate_limit)
			aggregate_next_send = std::max(aggregate_next_send, clock::now() - 2ms) +
			                      std::chrono::nanoseconds(uint64_t(result * 8e9 / aggregate_limit));

		if (carries_data(type))
		{
			send_times[nonce_counter % send_times.size()] = {nonce_counter, index, sent_time};
			if (transmissions.size() < 8192)
			{
				transmission sent{index, sent_time, wire.size()};
				if (reliable_kind(type))
				{
					sent.reliable = true;
					sent.message = id;
					sent.fragment = fragment;
				}
				if ((type == kind::data or type == kind::video) and deadline != clock::time_point{})
				{
					sent.video = type == kind::video;
					sent.message = id;
					sent.fragment = fragment;
					sent.count = count;
					sent.payload.assign(payload.begin(), payload.end());
					sent.deadline = deadline;
				}
				transmissions.emplace(nonce_counter, std::move(sent));
			}
			++p.sent;
			++group_sent;
		}

		if (type == kind::video and fec_active(p))
		{
			const auto now = clock::now();
			if (not p.fec_out.empty() and now >= p.fec_deadline)
				p.fec_out.clear();
			if (p.fec_out.empty())
				p.fec_deadline = deadline == clock::time_point{} ? now + 10ms : deadline;
			else if (deadline != clock::time_point{})
				p.fec_deadline = std::min(p.fec_deadline, deadline);

			p.fec_out.push_back({nonce_counter, std::move(plain)});
			if (p.fec_out.size() == options.fec_group_size)
			{
				const auto parity = fec_encode(p.fec_out);
				p.fec_out.clear();
				if (now + std::chrono::duration<double, std::milli>(earliest_arrival_delay_ms(p)) < p.fec_deadline and
				    transmit(index, kind::parity, 0, 0, 0, parity))
				{
					const auto duration = std::chrono::nanoseconds(uint64_t((parity.size() + 41) * 8e9 / p.stats.capacity_bps));
					p.next_send += duration;
					p.finish += double(parity.size() + 41) * 8 / p.stats.capacity_bps;
				}
			}
		}
		return true;
	}

	bool unknown_path(size_t socket_index, const socket_address & peer) const
	{
		return std::ranges::none_of(paths, [&](const auto & path) { return path.socket_index == socket_index and path.peer == peer; });
	}

	void receive(size_t socket_index)
	{
		std::array<uint8_t, 1280> wire;
		for (size_t batch = 0; batch < 256; ++batch)
		{
			socket_address peer;
			peer.size = sizeof(peer.address);
			const auto length = recvfrom(sockets[socket_index]->fd,
			                             wire.data(),
			                             wire.size(),
			                             MSG_DONTWAIT | MSG_TRUNC,
			                             reinterpret_cast<sockaddr *>(&peer.address),
			                             &peer.size);
			if (length < 0)
				break;
			if (length < 41 or size_t(length) > wire.size() or not std::equal(magic.begin(), magic.end(), wire.begin()))
				continue;
			if (not discover and unknown_path(socket_index, peer))
				continue;
			if (single_path and
			    (paths[*single_path].socket_index != socket_index or not(paths[*single_path].peer == peer)))
				continue;

			std::span<const uint8_t> header(wire.data() + 4, 8);
			const auto nonce_counter = read(header, 8);
			if (replay.contains(nonce_counter))
				continue;

			const auto nonce = make_nonce(not server, nonce_counter);
			std::array<uint8_t, 1280> plain;
			int bytes = 0, final = 0;
			if (EVP_DecryptInit_ex(decrypt.get(), EVP_aes_256_gcm(), nullptr, key.data(), nonce.data()) != 1 or
			    EVP_DecryptUpdate(decrypt.get(), nullptr, &bytes, wire.data(), 12) != 1 or
			    EVP_DecryptUpdate(decrypt.get(), plain.data(), &bytes, wire.data() + 12, length - 28) != 1 or
			    EVP_CIPHER_CTX_ctrl(decrypt.get(), EVP_CTRL_GCM_SET_TAG, 16, wire.data() + length - 16) != 1 or
			    EVP_DecryptFinal_ex(decrypt.get(), plain.data() + bytes, &final) != 1)
				continue;
			if (bytes < 13)
				continue;
			replay.insert(nonce_counter);

			std::span<const uint8_t> data(plain.data(), bytes);
			const auto type = kind(read(data, 1));
			const auto id = read(data, 8);
			const uint16_t fragment = read(data, 2), count = read(data, 2);
			const size_t index = find_path(socket_index, peer);
			if (index == max_paths)
				continue;

			auto & p = paths[index];
			const auto now = clock::now();
			if (now - p.seen > return_gap)
				p.resumed = now;
			p.seen = now;
			p.stats.received_bytes += length;

			if (type == kind::ping or type == kind::pong)
			{
				if (data.size() > 64)
					continue;

				p.stats.remote.interface.assign(reinterpret_cast<const char *>(data.data()), data.size());
				if (type == kind::ping)
				{
					const auto & name = p.stats.local.interface;
					transmit(index, kind::pong, id, 0, 0, {reinterpret_cast<const uint8_t *>(name.data()), name.size()});
				}
				else if (auto probe = p.probes.find(id); probe != p.probes.end() and now >= probe->second)
				{
					const double sample = std::chrono::duration<double, std::milli>(now - probe->second).count();
					p.probes.erase(probe);
					p.stats.jitter_ms = p.validated ? 0.8 * p.stats.jitter_ms + 0.2 * std::abs(sample - p.stats.rtt_ms) : 0;
					p.stats.rtt_ms = p.validated ? 0.8 * p.stats.rtt_ms + 0.2 * sample : sample;

					while (not p.rtt_window.empty() and p.rtt_window.back().second >= sample)
						p.rtt_window.pop_back();
					p.rtt_window.emplace_back(now, sample);
					while (p.rtt_window.front().first < now - 10s)
						p.rtt_window.pop_front();

					p.validated = true;
					p.stats.ever_active = true;
					p.confirmed = now;
					last_received = now;
				}
				continue;
			}

			if (not p.validated)
				continue;

			if (type == kind::video)
				remember_fec(nonce_counter, {plain.data(), size_t(bytes)}, now);
			if (type == kind::parity)
			{
				if (data.size() <= 1200 and fec_pending.size() < 256)
					fec_pending.push_back({now, index, {data.begin(), data.end()}});
				continue;
			}

			if (type == kind::ack)
			{
				if (data.size() != 24 + ack_window / 8)
					continue;

				const uint64_t highest = read(data, 8);
				const uint64_t sample = read(data, 8);
				const auto arrival = int64_t(read(data, 8));
				if (highest > counter)
					continue;

				if (const auto & record = send_times[sample % send_times.size()];
				    sample and record.nonce == sample and record.time >= paths[record.path_index].created)
					add_delay_sample(paths[record.path_index], arrival - nanoseconds(record.time), now);

				const uint64_t lowest = highest - std::min<uint64_t>(highest, ack_window - 1);
				for (auto it = transmissions.lower_bound(lowest); it != transmissions.end() and it->first <= highest;)
				{
					const uint64_t bit = highest - it->first;
					if (not((data[bit / 8] >> (bit % 8)) & 1))
					{
						++it;
						continue;
					}

					auto & sent_path = paths[it->second.path_index];
					// Reordered after it was reported missing
					if (it->second.missing)
					{
						sent_path.lost -= sent_path.lost > 0;
						group_lost -= group_lost > 0;
					}
					sent_path.acknowledged += it->second.bytes;
					if (impairment)
						sent_path.ack_age_max_ms = std::max(sent_path.ack_age_max_ms,
						                                    std::chrono::duration<double, std::milli>(now - it->second.time).count());
					sent_path.latest_acked_send = std::max(sent_path.latest_acked_send, it->second.time);
					group_acked += it->second.bytes;

					if (it->second.reliable)
						if (auto message = reliable.find(it->second.message);
						    message != reliable.end() and it->second.fragment < message->second.acknowledged_fragments.size())
							message->second.acknowledged_fragments[it->second.fragment] = true;
					it = transmissions.erase(it);
				}

				// What is left in the reported range had not arrived; allow 1 ms of reordering. Counted
				// lost now, in the controller interval that caused it.
				for (auto it = transmissions.lower_bound(lowest); it != transmissions.end() and it->first <= highest; ++it)
					if (auto & sent_path = paths[it->second.path_index];
					    not it->second.missing and it->second.time + 1ms < sent_path.latest_acked_send)
					{
						it->second.missing = true;
						++sent_path.lost;
						++group_lost;
					}
			}
			else if (type == kind::check_delivery)
			{
				const auto expected = expected_for(id);
				if (id < expected or ordered.contains(id))
					acknowledge_delivery(index, id);
				else
					transmit(index, kind::retry, id, 0, 0);
			}
			else if (type == kind::retry)
			{
				if (auto message = reliable.find(id); message != reliable.end())
				{
					std::fill(message->second.acknowledged_fragments.begin(), message->second.acknowledged_fragments.end(), false);
					message->second.cursor = 0;
					message->second.sent = {};
				}
			}
			else if (type == kind::delivered)
			{
				if (data.size() == 8)
				{
					const uint64_t credit = read(data, 8);
					const auto issued = id & bulk_bit ? bulk_id : id & video_bit ? video_id
					                                                             : reliable_id;
					if ((credit & channel_bits) == (id & channel_bits) and credit <= issued + 1)
					{
						peer_expected[channel_index(id)] = std::max(peer_expected[channel_index(id)], credit);
						std::erase_if(reliable, [&](const auto & item) {
							if ((item.first & channel_bits) != (id & channel_bits) or item.first >= credit)
								return false;
							release_reliable(item.second);
							return true;
						});
					}
				}
				if (auto it = reliable.find(id); it != reliable.end())
				{
					release_reliable(it->second);
					reliable.erase(it);
				}
			}
			else if (type == kind::endpoint and discover)
			{
				try
				{
					auto packet = owned_packet(data);
					auto endpoint = packet.deserialize<network_endpoint>();
					parse_endpoint(endpoint);
					if (not endpoint.port or endpoint.address.size() > 64 or endpoint.interface.size() > 64)
						continue;

					if (auto existing = std::ranges::find(remote_endpoints, endpoint); existing != remote_endpoints.end())
						remote_seen[existing - remote_endpoints.begin()] = now;
					else if (remote_endpoints.size() < 32)
					{
						remote_endpoints.push_back(std::move(endpoint));
						remote_seen.push_back(now);
						add_candidates();
					}
				}
				catch (const std::exception &)
				{}
			}
			else if (carries_data(type))
			{
				accepted.insert(nonce_counter);
				p.ack_repeats = 3;
				p.ack_sample = nonce_counter;
				p.ack_sample_arrival = now;

				if (type == kind::video or type == kind::reliable_video)
				{
					if (data.size() < 8)
						continue;

					const auto sequence = read(data, 8);
					if (not p.received_video.contains(sequence))
					{
						p.received_video.insert(sequence);
						++p.stats.received_video_packets;
						p.stats.video_gaps = p.received_video.highest - p.stats.received_video_packets;
						p.stats.incoming_loss = double(p.stats.video_gaps) / p.received_video.highest;
					}
				}
				accept_fragment(index, type, id, fragment, count, data, now);
			}
		}
	}

	// Path timing and congestion control

	double arrival_delay_ms(const path & p) const
	{
		return p.stats.rtt_ms / 2 + (options.arrival_scheduling ? 2 * p.stats.jitter_ms + 1 : 0);
	}

	double earliest_arrival_delay_ms(const path & p) const
	{
		// Jitter can rank paths, but cannot prove that a fresh packet will be late.
		return (p.rtt_window.empty() ? p.stats.rtt_ms : p.rtt_window.front().second) / 2;
	}

	// Queueing towards the peer, unlike the round trip, does not include a held return path.
	static void add_delay_sample(path & p, int64_t delay_ns, clock::time_point now)
	{
		while (not p.delay_window.empty() and p.delay_window.back().second >= delay_ns)
			p.delay_window.pop_back();
		p.delay_window.emplace_back(now, delay_ns);
		while (p.delay_window.front().first < now - 10s)
			p.delay_window.pop_front();

		p.latest_queue_delay_ms = (delay_ns - p.delay_window.front().second) / 1e6;
		p.queue_delay_ms = p.delay_sampled == clock::time_point{} ? p.latest_queue_delay_ms : 0.9 * p.queue_delay_ms + 0.1 * p.latest_queue_delay_ms;
		p.delay_sampled = now;
	}

	// Acknowledgements for packets sent since this time would have arrived by now: a path kept
	// receiving from the peer without a pause, and each acknowledgement covers every path
	clock::time_point hearing_since(clock::time_point now) const
	{
		auto result = clock::time_point::max();
		for (const auto & p: paths)
			if (now - p.seen < return_gap)
				result = std::min(result, p.resumed);
		return result;
	}

	// Reserves the path's send time for these bytes and returns it
	std::chrono::nanoseconds pace(path & p, size_t bytes, clock::time_point now)
	{
		const auto duration = std::chrono::nanoseconds(uint64_t(bytes * 8e9 / p.stats.capacity_bps));
		p.next_send = std::max(p.next_send, now - 2ms) + duration;
		p.finish += double(bytes) * 8 / p.stats.capacity_bps;
		return duration;
	}

	// Video paths and fragment scheduling

	// Paths that may carry video: shards sent over a much slower path complete their frame after
	// the next one, and the client then abandons it. Paths join within 2 ms of the fastest usable
	// path and leave beyond 3 ms; paths with little capacity left do not set the reference.
	std::vector<size_t> video_members;

	void update_video_paths(const std::vector<size_t> & candidates)
	{
		std::vector<size_t> members;
		if (not candidates.empty())
		{
			const uint64_t best_capacity = std::ranges::max(candidates | std::views::transform([&](size_t i) { return paths[i].stats.capacity_bps; }));
			double fastest = 1e9;
			for (size_t i: candidates)
				if (paths[i].stats.capacity_bps * 4 >= best_capacity)
					fastest = std::min(fastest, arrival_delay_ms(paths[i]));

			for (size_t i: candidates)
			{
				const double limit = fastest + (std::ranges::contains(video_members, i) ? 3 : 2);
				if (arrival_delay_ms(paths[i]) <= limit)
					members.push_back(i);
			}
		}
		video_members = std::move(members);
	}

	static kind message_kind(const message & m)
	{
		if (m.reliable_video)
			return kind::reliable_video;
		if (m.bulk)
			return kind::bulk;
		if (m.reliable)
			return kind::reliable;
		if (m.video)
			return kind::video;
		return kind::data;
	}

	bool send_fragment(message & m, const std::vector<size_t> & candidates, clock::time_point now)
	{
		if (candidates.empty() or (m.bulk and aggregate_next_send > now))
			return false;

		const size_t fragments = (m.data.size() + fragment_size - 1) / fragment_size;
		if (m.reliable)
		{
			while (m.cursor < fragments and m.acknowledged_fragments[m.cursor])
				++m.cursor;
			if (m.cursor == fragments)
			{
				transmit(candidates.front(), kind::check_delivery, m.id, 0, 0);
				m.cursor = 0;
				m.sent = now;
				return true;
			}
		}

		const size_t fragment = m.cursor;
		{
			const auto payload = std::span<const uint8_t>(m.data).subspan(fragment * fragment_size,
			                                                              std::min(fragment_size, m.data.size() - fragment * fragment_size));
			const auto expected_finish = [&](const path & q) {
				return q.finish + (options.arrival_scheduling ? (q.stats.rtt_ms / 2 + 2 * q.stats.jitter_ms) / 1000 : 0);
			};
			size_t best = candidates.front();
			for (const size_t i: candidates)
			{
				const auto & p = paths[i];
				const auto & chosen = paths[best];
				if (m.reliable ? p.stats.rtt_ms < chosen.stats.rtt_ms : expected_finish(p) < expected_finish(chosen))
					best = i;
			}

			auto & selected = paths[best];
			const auto duration = pace(selected, payload.size() + 64, now);
			const auto control_share = m.bulk and (not pending.empty() or not video.empty()) ? 4 : 1;
			if (m.reliable)
				selected.next_control = std::max(selected.next_control, now - 2ms) + duration * control_share;
			transmit(best, message_kind(m), m.id, fragment, fragments, payload, m.deadline);

			if ((m.reliable or m.protect or m.data.size() < 512) and candidates.size() > 1)
			{
				const auto alternate = *std::ranges::find_if(candidates, [best](size_t i) { return i != best; });
				transmit(alternate, message_kind(m), m.id, fragment, fragments, payload);
				auto & other = paths[alternate];
				const auto copy_duration = pace(other, payload.size() + 64, now);
				if (m.reliable)
					other.next_control = std::max(other.next_control, now - 2ms) + copy_duration * control_share;
			}
		}

		if (++m.cursor != fragments)
			return false;
		m.sent = now;
		m.cursor = 0;
		return true;
	}

	// Reliable ordered delivery and reassembly

	uint64_t & expected_for(uint64_t id)
	{
		if (id & bulk_bit)
			return expected_bulk;
		if (id & video_bit)
			return expected_video;
		return expected_reliable;
	}

	void release_reliable(const message & m)
	{
		queued_bytes -= m.data.size();
		if (m.bulk)
			bulk_bytes -= m.data.size();
		if (m.reliable_video)
			reliable_video_bytes -= m.data.size();
	}

	void queue_received(std::vector<uint8_t> data, size_t path_index)
	{
		if (received.size() >= 4096 or data.size() > memory_limit - receive_bytes)
			throw std::runtime_error("Multilink receive queue exceeded");

		const bool notify = received.empty();
		receive_bytes += data.size();
		received.emplace_back(std::move(data), path_index);
		if (notify)
			signal(ready);
	}

	void acknowledge_delivery(size_t index, uint64_t id)
	{
		std::vector<uint8_t> credit;
		append(credit, expected_for(id), 8);
		transmit(index, kind::delivered, id, 0, 0, credit);
	}

	void accept_fragment(size_t index, kind type, uint64_t id, uint16_t fragment, uint16_t count, std::span<const uint8_t> bytes, clock::time_point now)
	{
		const bool reliable_packet = reliable_kind(type);
		const bool essential = type == kind::reliable and not(id & channel_bits);
		const size_t assembly_limit = essential ? memory_limit : memory_limit - 2 * 1024 * 1024;
		auto & expected = expected_for(id);
		if (not id or not count or count > (max_message + fragment_size - 1) / fragment_size or fragment >= count)
			return;

		if (reliable_packet)
		{
			if (id < expected or ordered.contains(id))
			{
				acknowledge_delivery(index, id);
				return;
			}
			if (id - expected >= 1024)
				return;
		}
		else if (delivered.contains(id))
			return;

		const auto identity = std::make_pair(reliable_packet, id);
		if (not assemblies.contains(identity))
		{
			const size_t metadata = count * sizeof(std::vector<uint8_t>);
			if (assemblies.size() >= (essential ? 1024 : 896) or metadata > assembly_limit - std::min(assembly_limit, assembly_bytes))
				return;
			assemblies.emplace(identity, assembly{now, std::vector<std::vector<uint8_t>>(count)});
			assembly_bytes += metadata;
		}

		auto & a = assemblies.at(identity);
		if (a.fragments.size() != count or bytes.empty())
			return;
		if (reliable_packet)
			a.created = now;
		if (not a.fragments[fragment].empty())
			return;
		if (bytes.size() > assembly_limit - std::min(assembly_limit, assembly_bytes) or bytes.size() > max_message - a.bytes)
			return;

		a.fragments[fragment].assign(bytes.begin(), bytes.end());
		if (reliable_packet)
			a.created = now;
		a.bytes += bytes.size();
		assembly_bytes += bytes.size();
		if (++a.received != count)
			return;

		std::vector<uint8_t> data;
		data.reserve(a.bytes);
		for (auto & part: a.fragments)
			data.insert(data.end(), part.begin(), part.end());
		assembly_bytes -= a.bytes + a.fragments.size() * sizeof(std::vector<uint8_t>);
		assemblies.erase(identity);

		if (reliable_packet)
		{
			if (data.size() > memory_limit - ordered_bytes)
				throw std::runtime_error("Multilink control reorder queue exceeded");
			ordered_bytes += data.size();
			ordered.emplace(id, std::move(data));
			for (auto it = ordered.find(expected); it != ordered.end(); it = ordered.find(expected))
			{
				ordered_bytes -= it->second.size();
				queue_received(std::move(it->second), index);
				ordered.erase(it);
				++expected;
			}
			acknowledge_delivery(index, id);
		}
		else
		{
			delivered.insert(id);
			queue_received(std::move(data), index);
		}
	}

	// FEC repair

	bool fec_active(const path & p) const
	{
		return options.fec_group_size and
		       p.stats.loss > 0.005 and
		       p.stats.loss < 0.1 and
		       p.queue_delay_ms < 5 and
		       p.stats.sent_bps < p.stats.capacity_bps * 0.95;
	}

	// Keeps a received video packet for 50 ms, to rebuild a lost one from parity
	void remember_fec(uint64_t sequence, std::span<const uint8_t> bytes, clock::time_point now)
	{
		if (fec_received.size() < 8192)
		{
			fec_received.insert_or_assign(sequence, std::vector<uint8_t>(bytes.begin(), bytes.end()));
			fec_expiry.emplace_back(now + 50ms, sequence);
		}
	}

	// Periodic work

	void tick(clock::time_point now)
	{
		if (impairment)
			impairment->pump(now, [this](const auto & destination, auto bytes) { return send_impaired_wire(destination, bytes); });

		if (refresh and now - last_refresh >= 1s)
		{
			last_refresh = now;
			if (generation != network_generation())
			{
				generation = network_generation();
				for (auto & s: sockets)
					if (s->fd >= 0)
					{
						close(s->fd);
						s->fd = -1;
					}
			}

			for (size_t i = remote_endpoints.size(); i > 0; --i)
				if (now - remote_seen[i - 1] > 5s)
				{
					remote_endpoints.erase(remote_endpoints.begin() + i - 1);
					remote_seen.erase(remote_seen.begin() + i - 1);
				}

			const auto local = local_endpoints(server ? port : 0, include_loopback);
			for (auto & s: sockets)
			{
				if (s->fd < 0)
					continue;

				const auto listed = [&](const auto & endpoint) {
					return endpoint.address == s->local.address and
					       endpoint.interface == s->local.interface and
					       endpoint.scope == s->local.scope;
				};
				if (not std::ranges::any_of(local, listed))
				{
					close(s->fd);
					s->fd = -1;
				}
			}
			for (const auto & endpoint: local)
			{
				const auto bound = [&](const auto & s) {
					return s->fd >= 0 and s->local.address == endpoint.address and s->local.interface == endpoint.interface;
				};
				if (not std::ranges::any_of(sockets, bound))
					add_socket(endpoint);
			}
			add_candidates();
		}

		if (options.shared_bottleneck and now - aggregate_sample >= 200ms)
		{
			uint64_t capacity = 0;
			for (const auto & path: paths)
			{
				if (active(path, now))
					capacity += path.stats.capacity_bps;
			}

			const auto observed = uint64_t(group_acked * 8 / std::chrono::duration<double>(now - aggregate_sample).count());
			if (group_sent and group_lost > group_sent * 0.02 and observed)
				coupled_capacity_bps = std::min(capacity, std::max<uint64_t>(1'000'000, observed * 1.05));
			else if (coupled_capacity_bps and observed > coupled_capacity_bps * 0.7)
				coupled_capacity_bps = std::min(capacity, uint64_t(coupled_capacity_bps * 1.03));
			group_acked = group_sent = group_lost = 0;
			aggregate_sample = now;
		}

		for (size_t i = 0; i < paths.size(); ++i)
		{
			auto & p = paths[i];
			if (single_path and i != *single_path)
				continue;

			if (now - p.probe >= 100ms)
			{
				p.probe = now;
				p.probes.emplace(nanoseconds(now), now);
				std::erase_if(p.probes, [now](const auto & probe) { return now - probe.second > 2s; });
				const auto & name = p.stats.local.interface;
				transmit(i, kind::ping, nanoseconds(now), 0, 0, {reinterpret_cast<const uint8_t *>(name.data()), name.size()});
			}

			if (now - p.sample_start >= 200ms)
			{
				const double interval = std::chrono::duration<double>(now - p.sample_start).count();
				const double observed = p.acknowledged * 8 / interval;
				p.stats.sent_bps = (p.stats.sent_bytes - p.previous_sent) * 8 / interval;
				p.stats.received_bps = (p.stats.received_bytes - p.previous_received) * 8 / interval;
				p.previous_sent = p.stats.sent_bytes;
				p.previous_received = p.stats.received_bytes;
				p.stats.loss = p.sent ? std::min(1.0, double(p.lost) / p.sent) : 0;

				// A path without traffic gives no evidence; keep its estimate for when it is used again.
				const bool loaded = p.loaded = p.sent >= 20;
				const auto capacity_before = p.stats.capacity_bps;
				const char * action = "idle";
				const double queue_delay_ms = now - p.delay_sampled < 200ms ? p.queue_delay_ms : 0;
				const bool lossy = loaded and p.stats.loss > options.loss_threshold;

				// Loss shows the path is used to its limit: its estimate is no longer only a lower bound
				if (lossy)
					p.slow_start = false;

				bool queueing = false;
				// Loss without queueing comes from the radio, not from sending too much; repairs cover it.
				if (lossy and (queue_delay_ms > 2 or p.stats.loss > 0.1))
				{
					action = "loss";
					p.congested_bps = p.stats.capacity_bps;
					p.stats.capacity_bps = std::max<uint64_t>(1'000'000, p.stats.capacity_bps * 0.8);
					p.slow_start = false;
				}
				// A growing queue backs off too, but not below what the path delivered: Wi-Fi jitter
				// alone must not erase the estimate.
				else if (loaded and queue_delay_ms > 5 and p.latest_queue_delay_ms > 5)
				{
					action = "delay";
					queueing = true;
					p.congested_bps = p.stats.capacity_bps;
					if (not p.delay_floor_bps)
						p.delay_floor_bps = observed * 0.9;
					p.stats.capacity_bps = std::min<uint64_t>(p.stats.capacity_bps,
					                                          std::max({1'000'000.0, p.stats.capacity_bps * 0.8, double(p.delay_floor_bps)}));
					p.slow_start = false;
				}
				else if (observed > p.stats.capacity_bps * 0.7 * (fec_active(p) ? 0.8 : 1.0))
				{
					action = "grow";
					const double growth = p.slow_start ? 1.5 : p.stats.capacity_bps < p.congested_bps * 0.9 ? 1.15
					                                                                                        : 1.03;
					p.stats.capacity_bps = std::min<uint64_t>(1'000'000'000, p.stats.capacity_bps * growth);
				}
				if (not queueing)
					p.delay_floor_bps = 0;

				if (impairment)
					impairment->controller_sample(now,
					                              i,
					                              active(p, now),
					                              capacity_before,
					                              p.stats.capacity_bps,
					                              p.sent,
					                              p.lost,
					                              p.acknowledged,
					                              observed,
					                              p.stats.rtt_ms,
					                              queue_delay_ms,
					                              p.latest_queue_delay_ms,
					                              p.ack_age_max_ms,
					                              action);
				p.ack_age_max_ms = 0;
				p.acknowledged = p.lost = p.sent = 0;
				p.sample_start = now;
			}

			if (now - last_ack >= 2ms and p.ack_repeats)
			{
				// The newest packet number and which of the ones before it arrived, from any path
				std::vector<uint8_t> bytes;
				append(bytes, accepted.highest, 8);
				append(bytes, p.ack_sample, 8);
				append(bytes, nanoseconds(p.ack_sample_arrival), 8);
				bytes.resize(bytes.size() + ack_window / 8);
				for (size_t bit = 0; bit < ack_window; ++bit)
					if (accepted.seen[bit])
						bytes[24 + bit / 8] |= 1 << (bit % 8);
				transmit(i, kind::ack, 0, 0, 0, bytes);
				--p.ack_repeats;
			}
		}
		if (now - last_ack >= 2ms)
			last_ack = now;

		// A packet is lost once an acknowledgement reported it missing, or after a timeout while the
		// peer's packets kept arriving. While they are held, so are its acknowledgements: the packet
		// most likely arrived and is neither repaired nor counted lost.
		std::vector<uint64_t> repairs;
		const auto heard = hearing_since(now);
		for (auto & [nonce, sent]: transmissions)
		{
			const auto & path = paths[sent.path_index];
			const bool timed_out = sent.time >= heard and
			                       now - sent.time >= std::chrono::duration<double, std::milli>(std::max(20.0, 2 * path.stats.rtt_ms + 10));
			if ((sent.missing or timed_out) and not sent.payload.empty() and now < sent.deadline)
				repairs.push_back(nonce);
		}
		std::erase_if(transmissions, [&](const auto & entry) {
			const auto & sent = entry.second;
			auto & path = paths[sent.path_index];
			// Batched acknowledgements can arrive after a media repair deadline.
			const bool expired = now - sent.time >= std::chrono::duration<double, std::milli>(std::max(50.0, 2 * path.stats.rtt_ms + 10));
			if (sent.missing)
				return expired;
			if (sent.time >= heard and expired)
			{
				++path.lost;
				++group_lost;
				return true;
			}
			return now - sent.time >= 1s;
		});

		std::erase_if(assemblies, [&](const auto & entry) {
			if (now - entry.second.created < (entry.first.first ? 2s : 50ms))
				return false;
			assembly_bytes -= entry.second.bytes + entry.second.fragments.size() * sizeof(std::vector<uint8_t>);
			return true;
		});

		while (not fec_expiry.empty() and fec_expiry.front().first <= now)
		{
			fec_received.erase(fec_expiry.front().second);
			fec_expiry.pop_front();
		}
		std::erase_if(fec_pending, [&](const auto & item) {
			if (now - item.created > 25ms)
				return true;

			bool finished = false;
			const auto recovered = fec_recover(item.data, fec_received, &finished);
			if (not recovered)
				return finished;

			std::span<const uint8_t> data(recovered->bytes);
			if (data.size() < 21 or kind(read(data, 1)) != kind::video)
				return true;
			const auto id = read(data, 8);
			const auto fragment = read(data, 2), count = read(data, 2);
			read(data, 8);

			++paths[item.path].stats.recovered_fragments;
			accepted.insert(recovered->sequence);
			accept_fragment(item.path, kind::video, id, fragment, count, data, now);
			remember_fec(recovered->sequence, recovered->bytes, now);
			return true;
		});

		auto candidates = usable(now);
		for (uint64_t nonce: repairs)
		{
			auto entry = transmissions.find(nonce);
			if (entry == transmissions.end())
				continue;

			// Copy: transmit() records the repair while the original keeps its payload.
			const transmission repair = entry->second;
			// Prefer another path; the original path is the fallback, unless nothing sent on it since
			// was acknowledged and another path can take over its traffic.
			const bool failing = paths[repair.path_index].latest_acked_send < repair.time;
			const auto & allowed = repair.video ? video_members : candidates;
			std::vector<size_t> order;
			for (size_t i: allowed)
				if (i != repair.path_index and std::ranges::contains(candidates, i))
					order.push_back(i);
			if (std::ranges::contains(candidates, repair.path_index) and not(failing and not order.empty()))
				order.push_back(repair.path_index);

			for (size_t i: order)
			{
				auto & path = paths[i];
				// Repairs go before new data but may use only a fifth of the path's rate, except those
				// carrying a failing path's traffic
				const bool credited = not failing or i == repair.path_index;
				path.repair_credit = std::min(64.0 * 1024,
				                              path.repair_credit +
				                                      std::chrono::duration<double>(now - path.repair_refill).count() * path.stats.capacity_bps / 8 * 0.2);
				path.repair_refill = now;

				const bool paced = aggregate_next_send > now or path.next_send > now;
				const bool out_of_credit = credited and path.repair_credit < repair.bytes;
				const bool too_late = now + std::chrono::duration<double, std::milli>(earliest_arrival_delay_ms(path)) >= repair.deadline;
				if (paced or out_of_credit or too_late)
					continue;
				if (not transmit(i,
				                 repair.video ? kind::video : kind::data,
				                 repair.message,
				                 repair.fragment,
				                 repair.count,
				                 repair.payload,
				                 repair.deadline))
					continue;

				transmissions.at(nonce).payload.clear();
				if (credited)
					path.repair_credit -= repair.bytes;
				++path.stats.repairs_sent;
				pace(path, repair.bytes, now);
				break;
			}
		}

		if (refresh and not candidates.empty() and now - last_advertisement >= 1s)
		{
			last_advertisement = now;
			for (const auto & s: sockets)
			{
				if (s->fd < 0)
					continue;
				serialization_packet packet;
				packet.serialize(s->local);
				const auto bytes = flatten(packet);
				transmit(candidates.front(), kind::endpoint, 0, 0, 0, bytes);
			}
		}

		for (auto & [_, m]: reliable)
		{
			if (now > m.deadline)
				throw std::runtime_error("Multilink reliable delivery timed out");

			const uint64_t peer_next = peer_expected[channel_index(m.id)];
			const bool waits_for_control = (m.bulk or m.reliable_video) and peer_expected[0] <= m.control_dependency;
			const bool beyond_window = m.id >= peer_next and m.id - peer_next >= 1024;
			if (waits_for_control or beyond_window)
				continue;

			auto retry_delay = candidates.empty()
			                           ? 60ms
			                           : std::chrono::duration_cast<std::chrono::milliseconds>(
			                                     std::chrono::duration<double, std::milli>(std::max(10.0, 2 * paths[candidates.front()].stats.rtt_ms + 5)));
			// Nothing from the peer since the send: acknowledgements held on the way back are no reason
			// to send everything again soon
			if (std::ranges::none_of(paths, [&](const path & p) { return p.seen > m.sent; }))
				retry_delay = std::max<std::chrono::milliseconds>(retry_delay, 100ms);
			if (m.cursor == 0 and now - m.sent < retry_delay)
				continue;

			for (size_t batch = 0; batch < 8; ++batch)
			{
				std::vector<size_t> available;
				for (size_t i: candidates)
					if ((not m.bulk or paths[i].next_send <= now) and paths[i].next_control <= now)
						available.push_back(i);
				if (available.empty() or send_fragment(m, available, now))
					break;
			}
		}

		size_t pending_skip = 0;
		update_video_paths(candidates);
		const auto & video_candidates = video_members;
		for (size_t batch = 0; batch < 256 and (not pending.empty() or not video.empty()); ++batch)
		{
			auto & queue = pending.empty() ? video : pending;
			auto & m = queue.front();
			const bool impossible = not candidates.empty() and std::ranges::none_of(m.video ? video_candidates : candidates, [&](size_t i) {
				const auto earliest = std::max(now, std::max(paths[i].next_send, aggregate_next_send));
				return earliest + std::chrono::duration<double, std::milli>(earliest_arrival_delay_ms(paths[i])) < m.deadline;
			});
			if (now >= m.deadline or impossible)
			{
				if (m.video)
				{
					++counters.expired_video;
					if (video_drop_observer)
						video_drop_observer({m.data,
						                     now >= m.deadline ? video_queue_drop_reason::deadline : video_queue_drop_reason::arrival_impossible,
						                     m.created,
						                     now,
						                     m.deadline,
						                     m.target_deadline,
						                     m.cursor,
						                     queued_bytes});
				}
				queued_bytes -= m.data.size();
				if (&queue == &pending and pending_skip)
					--pending_skip;
				queue.pop_front();
				continue;
			}

			if (aggregate_next_send > now)
				break;

			const auto arrival = [&](size_t i) { return now + std::chrono::duration<double, std::milli>(earliest_arrival_delay_ms(paths[i])); };
			std::vector<size_t> idle;
			for (size_t i: candidates)
				if (paths[i].next_send <= now)
					idle.push_back(i);
			if (idle.empty())
				break;

			const auto earliest = std::ranges::min(idle | std::views::transform(arrival));
			// Unreliable data is unordered, so an idle path takes the oldest message it can still
			// deliver instead of waiting behind one only a busier path can. Within a tick the idle
			// set only shrinks, so skipped messages stay undeliverable. Video keeps frame order:
			// the client abandons a frame once the next one completes.
			while (pending_skip < pending.size() and pending[pending_skip].deadline <= earliest)
				++pending_skip;
			auto & source = pending_skip < pending.size() ? pending : video;
			const auto it = &source == &pending ? pending.begin() + pending_skip : video.begin();
			if (it == source.end() or it->deadline <= earliest)
				break;

			std::vector<size_t> available;
			for (size_t i: idle)
				if (arrival(i) < it->deadline and (not it->video or std::ranges::contains(video_candidates, i)))
					available.push_back(i);
			if (available.empty())
				break;

			if (send_fragment(*it, available, now))
			{
				queued_bytes -= it->data.size();
				source.erase(it);
			}
		}

		if (not candidates.empty())
		{
			const double floor = paths[*std::ranges::min_element(candidates, {}, [&](size_t i) { return paths[i].finish; })].finish;
			for (auto & p: paths)
				p.finish = std::max(0.0, p.finish - floor);
		}

		if (now - last_received > 10s and now - started > 10s)
			throw std::runtime_error("All multilink paths timed out");
	}

	// Queueing outgoing messages

	size_t send(serialization_packet & packet,
	            bool reliable_packet,
	            bool video_packet = false,
	            clock::time_point deadline = {},
	            bool bulk_packet = false,
	            bool protect = false)
	{
		auto bytes = flatten(packet);
		if (bytes.empty())
			throw std::invalid_argument("Empty multilink message");

		std::lock_guard lock(mutex);
		if (error)
			std::rethrow_exception(error);

		if (bulk_packet and
		    (bulk_bytes + bytes.size() > 4 * 1024 * 1024 or
		     std::ranges::count_if(reliable, [](const auto & item) { return item.second.bulk; }) >= 64))
		{
			++counters.rejected_bulk;
			return 0;
		}

		const bool reliable_video = reliable_packet and video_packet;
		const auto report_rejected_video = [&] {
			if (video_packet and video_drop_observer)
			{
				const auto now = clock::now();
				video_drop_observer({bytes, video_queue_drop_reason::queue_full, now, now, {}, deadline, 0, queued_bytes});
			}
		};
		if (reliable_video and
		    (reliable_video_bytes + bytes.size() > 8 * 1024 * 1024 or
		     std::ranges::count_if(reliable, [](const auto & item) { return item.second.reliable_video; }) >= 4096))
		{
			++counters.rejected_video;
			report_rejected_video();
			return 0;
		}

		const size_t essential_count = std::ranges::count_if(reliable, [](const auto & item) {
			return not item.second.bulk and not item.second.reliable_video;
		});
		const bool essential = reliable_packet and not bulk_packet and not reliable_video;
		const size_t limit = essential ? memory_limit : memory_limit - 2 * 1024 * 1024;
		const bool over_memory = bytes.size() > limit - std::min(limit, queued_bytes);
		const bool over_count = reliable_packet ? essential and essential_count >= 1024 : pending.size() + video.size() >= 4096;
		if (over_memory or over_count)
		{
			if (bulk_packet or reliable_video)
			{
				if (reliable_video)
					++counters.rejected_video;
				else
					++counters.rejected_bulk;
				report_rejected_video();
				return 0;
			}
			if (reliable_packet)
				throw std::runtime_error("Multilink control queue exceeded: " + std::to_string(reliable.size()) + " messages, " +
				                         std::to_string(queued_bytes) + " bytes");
			report_rejected_video();
			return 0;
		}

		if (reliable_id >= video_bit - 1 or
		    message_id >= video_bit - 1 or
		    (bulk_id & (video_bit - 1)) >= video_bit - 1 or
		    (video_id & (video_bit - 1)) >= video_bit - 1)
			throw std::runtime_error("Multilink message identifiers exhausted");

		const size_t size = bytes.size();
		queued_bytes += size;
		message m{reliable_video ? ++video_id : bulk_packet ? ++bulk_id
		                                : reliable_packet   ? ++reliable_id
		                                                    : ++message_id,
		          std::move(bytes),
		          clock::now(),
		          {},
		          reliable_packet};
		m.bulk = bulk_packet;
		m.video = video_packet;
		m.protect = protect;
		m.reliable_video = reliable_video;
		if (reliable_video)
			reliable_video_bytes += size;
		m.control_dependency = reliable_id;
		if (bulk_packet)
			bulk_bytes += size;
		m.deadline = deadline == clock::time_point{} ? m.created + 25ms : deadline;
		m.target_deadline = deadline;

		// Allow the bounded control queue to drain at the minimum reserved rate.
		if (reliable_packet)
		{
			m.deadline = m.created + 10s + std::chrono::milliseconds(queued_bytes * 24'000 / 250'000);
			m.acknowledged_fragments.resize((m.data.size() + fragment_size - 1) / fragment_size);
		}

		if (reliable_packet)
			reliable.emplace(m.id, std::move(m));
		else if (video_packet)
			video.push_back(std::move(m));
		else
			pending.push_back(std::move(m));
		signal(wake);
		return size;
	}
};

multilink::multilink() = default;

multilink::multilink(bool server, key_type key, std::vector<network_endpoint> local, std::vector<network_endpoint> remote, bool refresh_interfaces, bool discover_endpoints) :
        impl(std::make_unique<implementation>(server, key, std::move(local), std::move(remote), refresh_interfaces, discover_endpoints)) {}

multilink::multilink(multilink &&) noexcept = default;
multilink & multilink::operator=(multilink &&) noexcept = default;
multilink::~multilink() = default;

multilink::key_type multilink::random_key()
{
	key_type key;
	if (RAND_bytes(key.data(), key.size()) != 1)
		throw std::runtime_error("Generating multilink session key failed");
	return key;
}

multilink::operator bool() const
{
	return bool(impl);
}

int multilink::get_fd() const
{
	return impl ? impl->ready : -1;
}

void multilink::shutdown()
{
	if (not impl)
		return;

	std::lock_guard lock(impl->mutex);
	impl->error = std::make_exception_ptr(std::runtime_error("Multilink session closed"));
	impl->worker.request_stop();
	implementation::signal(impl->ready);
	implementation::signal(impl->wake);
}

void multilink::prepare_fork()
{
	if (not impl)
		return;

	impl->worker.request_stop();
	implementation::signal(impl->wake);
	if (impl->worker.joinable())
		impl->worker.join();
}

void multilink::resume_after_fork()
{
	if (impl)
		impl->worker = std::jthread([p = impl.get()](std::stop_token stop) { p->run(stop); });
}

std::vector<network_endpoint> multilink::endpoints() const
{
	std::vector<network_endpoint> result;
	if (not impl)
		return result;

	std::lock_guard lock(impl->mutex);
	for (const auto & s: impl->sockets)
		if (s->fd >= 0)
			result.push_back(s->local);
	return result;
}

std::vector<link_statistics> multilink::statistics() const
{
	std::vector<link_statistics> result;
	if (not impl)
		return result;

	std::lock_guard lock(impl->mutex);
	const auto now = clock::now();
	for (const auto & p: impl->paths)
	{
		auto stats = p.stats;
		stats.active = impl->active(p, now);
		result.push_back(std::move(stats));
	}
	return result;
}

uint64_t multilink::capacity_bps() const
{
	if (not impl)
		return 0;

	std::lock_guard lock(impl->mutex);
	uint64_t result = 0;
	bool any = false, growing = false;
	for (size_t i: impl->usable(clock::now()))
		if (const auto & p = impl->paths[i]; std::ranges::contains(impl->video_members, i))
		{
			any = true;
			// A path still growing from its initial guess gives only a lower bound while it carries
			// traffic, and no evidence while it does not
			if (p.slow_start)
				growing = growing or p.loaded;
			else
				result += p.stats.capacity_bps * (impl->fec_active(p) ? 0.8 : 1.0);
		}
	if (not any)
		return impl->last_capacity_bps;

	const auto limit = impl->options.aggregate_limit_bps ? impl->options.aggregate_limit_bps : impl->coupled_capacity_bps;
	// A path still growing from its initial guess gives only a lower bound
	if (growing)
		result = limit;
	else if (limit)
		result = std::min(result, limit);
	impl->last_capacity_bps = result;
	return result;
}

size_t multilink::send_raw(serialization_packet && packet)
{
	return impl->send(packet, false);
}

size_t multilink::send_reply_raw(serialization_packet && packet)
{
	const auto bytes = flatten(packet);
	if (bytes.empty() or bytes.size() > fragment_size)
		throw std::invalid_argument("Multilink reply must fit in one datagram");

	std::lock_guard lock(impl->mutex);
	if (impl->error)
		std::rethrow_exception(impl->error);

	const auto path = impl->receive_paths.find(std::this_thread::get_id());
	if (path == impl->receive_paths.end() or not impl->active(impl->paths[path->second], clock::now()))
		return 0;
	// Clock synchronization assumes the request and response use the same path.
	return impl->transmit(path->second, kind::data, ++impl->message_id, 0, 1, bytes) ? bytes.size() : 0;
}

size_t multilink::send_video_raw(serialization_packet && packet, clock::time_point deadline)
{
	return impl->send(packet, false, true, deadline);
}

size_t multilink::send_reliable_raw(serialization_packet && packet)
{
	return impl->send(packet, true);
}

size_t multilink::send_bulk_raw(serialization_packet && packet)
{
	return impl->send(packet, true, false, {}, true);
}

size_t multilink::send_video_raw(serialization_packet && packet, clock::time_point deadline, bool protect)
{
	return impl->send(packet, protect, true, deadline, false, protect);
}

void multilink::set_options(multilink_options options)
{
	if (not impl)
		return;

	std::lock_guard lock(impl->mutex);
	if (not std::isfinite(options.loss_threshold) or options.loss_threshold < 0 or options.loss_threshold > 1)
		throw std::invalid_argument("Loss threshold must be between zero and one");
	if (options.fec_group_size != 0 and options.fec_group_size != 4)
		throw std::invalid_argument("FEC group size must be zero or four");
	impl->options = options;
	if (not options.shared_bottleneck)
		impl->coupled_capacity_bps = 0;
}

void multilink::set_video_queue_drop_observer(video_queue_drop_observer observer)
{
	if (not impl)
		return;

	std::lock_guard lock(impl->mutex);
	impl->video_drop_observer = observer;
}

bool multilink::set_single_interface(std::string_view interface)
{
	if (not impl)
		return false;

	std::lock_guard lock(impl->mutex);
	auto candidates = impl->usable(clock::now());
	std::erase_if(candidates, [&](size_t i) { return impl->paths[i].stats.local.interface != interface; });
	if (candidates.empty())
		return false;

	impl->single_path = impl->preferred_single_path(candidates);
	impl->refresh = impl->discover = false;
	implementation::signal(impl->wake);
	return true;
}

bool multilink::set_multilink_enabled(bool enabled)
{
	if (not impl)
		return false;

	std::lock_guard lock(impl->mutex);
	if (enabled)
	{
		impl->single_path.reset();
		impl->refresh = impl->discover = true;
		impl->last_refresh = impl->last_advertisement = {};
	}
	else
	{
		const auto candidates = impl->usable(clock::now());
		if (candidates.empty())
			return false;
		impl->single_path = impl->preferred_single_path(candidates);
		impl->refresh = impl->discover = false;
	}
	implementation::signal(impl->wake);
	return true;
}

multilink_diagnostics multilink::diagnostics() const
{
	if (not impl)
		return {};

	std::lock_guard lock(impl->mutex);
	auto result = impl->counters;
	result.queued_bytes = impl->queued_bytes;
	result.receiving_assemblies = impl->assemblies.size();
	for (const auto & [id, message]: impl->reliable)
	{
		if (id & bulk_bit)
			++result.bulk_messages;
		else if (id & video_bit)
			++result.protected_video_messages;
		else
		{
			++result.reliable_messages;
			result.oldest_control_ms = std::max(result.oldest_control_ms,
			                                    std::chrono::duration<double, std::milli>(clock::now() - message.created).count());
		}
	}
	return result;
}

size_t multilink::send_many_raw(std::span<serialization_packet> packets)
{
	size_t result = 0;
	for (auto & packet: packets)
		result += send_raw(std::move(packet));
	return result;
}

deserialization_packet multilink::receive_raw()
{
	if (not impl)
		return {};

	std::lock_guard lock(impl->mutex);
	if (impl->error)
		std::rethrow_exception(impl->error);
	if (impl->received.empty())
		return {};

	auto [bytes, path_index] = std::move(impl->received.front());
	impl->received.pop_front();
	if (impl->receive_paths.size() >= 16)
		impl->receive_paths.clear();
	impl->receive_paths[std::this_thread::get_id()] = path_index;
	impl->receive_bytes -= bytes.size();
	if (impl->received.empty())
		implementation::drain(impl->ready);
	return owned_packet(bytes);
}

deserialization_packet multilink::receive_pending()
{
	return receive_raw();
}
} // namespace wivrn
