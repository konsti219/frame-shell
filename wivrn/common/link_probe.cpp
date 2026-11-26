// SPDX-License-Identifier: GPL-3.0-or-later
#include "link_probe.h"
#include <algorithm>
#include <array>
#include <chrono>
#include <cstring>
#include <openssl/rand.h>
#include <poll.h>
#include <thread>
#include <unistd.h>

namespace wivrn
{
namespace
{
using clock = std::chrono::steady_clock;
using namespace std::chrono_literals;
constexpr std::array<uint8_t, 8> magic{'W', 'Y', 'R', 'M', 'P', 'R', 'O', 'B'};
struct probe_socket
{
	int fd;
	network_endpoint local;
	probe_socket(network_endpoint local) : fd(open_network_socket(local)), local(std::move(local))
	{
		int enabled = 1;
		setsockopt(fd, SOL_SOCKET, SO_TIMESTAMPNS, &enabled, sizeof(enabled));
	}
	~probe_socket()
	{
		close(fd);
	}
};
} // namespace

struct link_probe::implementation
{
	struct pending
	{
		std::unique_ptr<probe_socket> socket;
		network_endpoint remote;
		std::array<uint8_t, 16> request;
		clock::time_point sent{};
		std::chrono::system_clock::time_point sent_wall{};
		clock::time_point used{};
		clock::time_point measured{};
		std::optional<float> latency;
	};
	std::vector<pending> probes;
	uint64_t generation = network_generation();
};

link_probe::link_probe() : impl(std::make_unique<implementation>()) {}
link_probe::~link_probe() = default;
std::optional<float> link_probe::sample(const network_endpoint & local, const network_endpoint & remote)
{
	const auto now = clock::now();
	if (impl->generation != network_generation())
	{
		impl->probes.clear();
		impl->generation = network_generation();
	}
	std::erase_if(impl->probes, [now](const auto & probe) { return now - probe.used > 10s; });
	auto it = std::ranges::find_if(impl->probes, [&](const auto & probe) { return probe.remote == remote and probe.socket->local == local; });
	if (it == impl->probes.end())
	{
		if (impl->probes.size() >= 128)
			return {};
		try
		{
			implementation::pending probe{.socket = std::make_unique<probe_socket>(local), .remote = remote};
			std::copy(magic.begin(), magic.end(), probe.request.begin());
			impl->probes.push_back(std::move(probe));
			it = std::prev(impl->probes.end());
		}
		catch (const std::exception &)
		{
			return {};
		}
	}
	it->used = now;
	std::array<uint8_t, 17> response;
	socket_address source;
	source.size = sizeof(source.address);
	iovec buffer{response.data(), response.size()};
	alignas(cmsghdr) std::array<uint8_t, CMSG_SPACE(sizeof(timespec))> control{};
	msghdr incoming{};
	incoming.msg_name = &source.address;
	incoming.msg_namelen = source.size;
	incoming.msg_iov = &buffer;
	incoming.msg_iovlen = 1;
	incoming.msg_control = control.data();
	incoming.msg_controllen = control.size();
	const auto received = recvmsg(it->socket->fd, &incoming, MSG_DONTWAIT);
	if (received == 16 and source == parse_endpoint(remote) and std::equal(it->request.begin(), it->request.end(), response.begin()))
	{
		for (auto * header = CMSG_FIRSTHDR(&incoming); header; header = CMSG_NXTHDR(&incoming, header))
			if (header->cmsg_level == SOL_SOCKET and header->cmsg_type == SO_TIMESTAMPNS and header->cmsg_len >= CMSG_LEN(sizeof(timespec)))
			{
				timespec stamp;
				memcpy(&stamp, CMSG_DATA(header), sizeof(stamp));
				const auto arrival = std::chrono::system_clock::time_point(std::chrono::seconds(stamp.tv_sec) + std::chrono::nanoseconds(stamp.tv_nsec));
				const float latency = std::chrono::duration<float, std::milli>(arrival - it->sent_wall).count();
				if (latency >= 0 and latency < 2000)
				{
					it->latency = latency;
					it->measured = now;
				}
			}
	}
	if (now - it->measured > 3s)
		it->latency.reset();
	if (now - it->sent > 1s)
	{
		if (RAND_bytes(it->request.data() + 8, 8) != 1)
			return {};
		it->sent_wall = std::chrono::system_clock::now();
		if (send_network_packet(it->socket->fd, local, parse_endpoint(remote), it->request) == 16)
			it->sent = now;
	}
	return it->latency;
}

struct link_probe_responder::implementation
{
	uint16_t port;
	std::jthread worker;
	explicit implementation(uint16_t port) : port(port), worker([this](std::stop_token stop) { run(stop); }) {}
	~implementation()
	{
		worker.request_stop();
		worker.join();
	}
	void run(std::stop_token stop)
	{
		std::vector<std::unique_ptr<probe_socket>> sockets;
		clock::time_point refresh{};
		while (not stop.stop_requested())
		{
			if (clock::now() - refresh > 1s)
			{
				refresh = clock::now();
				try
				{
					const auto local = local_endpoints(port, true);
					std::erase_if(sockets, [&](const auto & socket) { return std::ranges::find(local, socket->local) == local.end(); });
					for (const auto & endpoint: local)
						if (not std::ranges::any_of(sockets, [&](const auto & socket) { return socket->local == endpoint; }))
							try
							{
								sockets.push_back(std::make_unique<probe_socket>(endpoint));
							}
							catch (const std::exception &)
							{}
				}
				catch (const std::exception &)
				{}
			}
			std::vector<pollfd> fds;
			for (const auto & socket: sockets)
				fds.push_back({.fd = socket->fd, .events = POLLIN});
			if (poll(fds.data(), fds.size(), 10) <= 0)
				continue;
			// Bound the response rate; probes never start a runtime or amplify a request.
			for (size_t i = 0; i < sockets.size(); ++i)
			{
				if (not(fds[i].revents & POLLIN))
					continue;
				std::array<uint8_t, 17> bytes;
				socket_address peer;
				peer.size = sizeof(peer.address);
				const auto count = recvfrom(sockets[i]->fd, bytes.data(), bytes.size(), MSG_DONTWAIT, reinterpret_cast<sockaddr *>(&peer.address), &peer.size);
				if (count != 16 or not std::equal(magic.begin(), magic.end(), bytes.begin()))
					continue;
				const auto result = send_network_packet(sockets[i]->fd, sockets[i]->local, peer, std::span(bytes).first(16));
				(void)result;
			}
		}
	}
};
link_probe_responder::link_probe_responder(uint16_t port) : impl(std::make_unique<implementation>(port)) {}
link_probe_responder::~link_probe_responder() = default;
} // namespace wivrn
