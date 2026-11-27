// SPDX-License-Identifier: GPL-3.0-or-later
#include "network.h"

#include <algorithm>
#include <arpa/inet.h>
#include <array>
#include <atomic>
#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <fcntl.h>
#include <ifaddrs.h>
#include <map>
#include <memory>
#include <mutex>
#include <net/if.h>
#include <ranges>
#include <stdexcept>
#include <string_view>
#include <system_error>
#include <unistd.h>
#ifdef __ANDROID__
#include <android/multinetwork.h>
#endif

namespace wivrn
{
namespace
{
std::mutex network_mutex;
std::map<std::string, uint64_t> network_handles;
std::atomic<uint64_t> generation = 0;
} // namespace

bool socket_address::operator==(const socket_address & other) const
{
	if (address.ss_family != other.address.ss_family)
		return false;
	if (address.ss_family == AF_INET)
	{
		const auto & a = reinterpret_cast<const sockaddr_in &>(address);
		const auto & b = reinterpret_cast<const sockaddr_in &>(other.address);
		return a.sin_port == b.sin_port and a.sin_addr.s_addr == b.sin_addr.s_addr;
	}
	const auto & a = reinterpret_cast<const sockaddr_in6 &>(address);
	const auto & b = reinterpret_cast<const sockaddr_in6 &>(other.address);
	return a.sin6_port == b.sin6_port and a.sin6_scope_id == b.sin6_scope_id and memcmp(&a.sin6_addr, &b.sin6_addr, sizeof(a.sin6_addr)) == 0;
}

socket_address parse_endpoint(const network_endpoint & endpoint)
{
	socket_address result;
	auto & v4 = reinterpret_cast<sockaddr_in &>(result.address);
	if (inet_pton(AF_INET, endpoint.address.c_str(), &v4.sin_addr) == 1)
	{
		v4.sin_family = AF_INET;
		v4.sin_port = htons(endpoint.port);
		result.size = sizeof(v4);
		return result;
	}
	auto & v6 = reinterpret_cast<sockaddr_in6 &>(result.address);
	if (inet_pton(AF_INET6, endpoint.address.c_str(), &v6.sin6_addr) != 1)
		throw std::invalid_argument("Invalid network endpoint");
	v6.sin6_family = AF_INET6;
	v6.sin6_port = htons(endpoint.port);
	v6.sin6_scope_id = endpoint.scope;
	result.size = sizeof(v6);
	return result;
}

network_endpoint describe_endpoint(const sockaddr * address, std::string interface)
{
	network_endpoint result{.interface = std::move(interface)};
	char text[INET6_ADDRSTRLEN];
	if (address->sa_family == AF_INET)
	{
		const auto & a = *reinterpret_cast<const sockaddr_in *>(address);
		inet_ntop(AF_INET, &a.sin_addr, text, sizeof(text));
		result.port = ntohs(a.sin_port);
	}
	else if (address->sa_family == AF_INET6)
	{
		const auto & a = *reinterpret_cast<const sockaddr_in6 *>(address);
		if (IN6_IS_ADDR_V4MAPPED(&a.sin6_addr))
			inet_ntop(AF_INET, &a.sin6_addr.s6_addr[12], text, sizeof(text));
		else
			inet_ntop(AF_INET6, &a.sin6_addr, text, sizeof(text));
		result.port = ntohs(a.sin6_port);
		result.scope = a.sin6_scope_id;
	}
	else
		throw std::invalid_argument("Unsupported network address family");
	result.address = text;
	return result;
}

// WIVRN_MULTILINK_INTERFACES: comma separated interface names; unset or empty allows every interface.
static bool interface_allowed(std::string_view name)
{
	const char * allowed = std::getenv("WIVRN_MULTILINK_INTERFACES");
	if (not allowed or not *allowed)
		return true;
	for (const auto part: std::views::split(std::string_view(allowed), ','))
	{
		std::string_view entry(part.begin(), part.end());
		entry.remove_prefix(std::min(entry.find_first_not_of(" \t"), entry.size()));
		entry.remove_suffix(entry.size() - std::min(entry.find_last_not_of(" \t") + 1, entry.size()));
		if (entry == name)
			return true;
	}
	return false;
}

std::vector<network_endpoint> local_endpoints(uint16_t port, bool include_loopback)
{
	ifaddrs * raw = nullptr;
	if (getifaddrs(&raw) != 0)
		throw std::system_error(errno, std::generic_category(), "Enumerating network interfaces");
	std::unique_ptr<ifaddrs, decltype(&freeifaddrs)> addresses(raw, freeifaddrs);
	std::vector<network_endpoint> result;
	for (auto * a = raw; a; a = a->ifa_next)
	{
		if (not a->ifa_addr or not(a->ifa_flags & IFF_UP) or (not include_loopback and (a->ifa_flags & IFF_LOOPBACK)))
			continue;
		if (a->ifa_addr->sa_family != AF_INET and a->ifa_addr->sa_family != AF_INET6)
			continue;
		if (not interface_allowed(a->ifa_name))
			continue;
		auto endpoint = describe_endpoint(a->ifa_addr, a->ifa_name);
		endpoint.port = port;
		if (std::ranges::find(result, endpoint) == result.end())
			result.push_back(std::move(endpoint));
		if (result.size() == 32)
			break;
	}
	return result;
}

int open_network_socket(const network_endpoint & local)
{
	const auto address = parse_endpoint(local);
	const int fd = socket(address.address.ss_family, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
	if (fd < 0)
		throw std::system_error(errno, std::generic_category(), "Opening network socket");
	try
	{
		int enabled = 1;
		if (address.address.ss_family == AF_INET6)
			setsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &enabled, sizeof(enabled));
#ifdef __ANDROID__
		std::lock_guard lock(network_mutex);
		if (auto i = network_handles.find(local.interface); i != network_handles.end())
			if (android_setsocknetwork(i->second, fd) != 0)
				throw std::system_error(errno, std::generic_category(), "Binding socket to Android network");
#endif
		if (::bind(fd, reinterpret_cast<const sockaddr *>(&address.address), address.size) != 0)
			throw std::system_error(errno, std::generic_category(), "Binding network source address");
		int buffer = 2 * 1024 * 1024;
		setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &buffer, sizeof(buffer));
		setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &buffer, sizeof(buffer));
		return fd;
	}
	catch (...)
	{
		close(fd);
		throw;
	}
}

void set_network_handle(std::string interface, uint64_t handle)
{
	std::lock_guard lock(network_mutex);
	if (auto existing = network_handles.find(interface); existing != network_handles.end() and existing->second == handle)
		return;
	network_handles[std::move(interface)] = handle;
	++generation;
}

ssize_t send_network_packet(int fd, const network_endpoint & local, const socket_address & peer, std::span<const uint8_t> data)
{
	iovec buffer{const_cast<uint8_t *>(data.data()), data.size()};
	msghdr outgoing{};
	outgoing.msg_name = const_cast<sockaddr_storage *>(&peer.address);
	outgoing.msg_namelen = peer.size;
	outgoing.msg_iov = &buffer;
	outgoing.msg_iovlen = 1;
	alignas(cmsghdr) std::array<uint8_t, CMSG_SPACE(sizeof(in6_pktinfo))> control{};
	const unsigned interface = if_nametoindex(local.interface.c_str());
	if (interface)
	{
		outgoing.msg_control = control.data();
		outgoing.msg_controllen = peer.address.ss_family == AF_INET ? CMSG_SPACE(sizeof(in_pktinfo)) : CMSG_SPACE(sizeof(in6_pktinfo));
		auto * header = CMSG_FIRSTHDR(&outgoing);
		header->cmsg_level = peer.address.ss_family == AF_INET ? IPPROTO_IP : IPPROTO_IPV6;
		header->cmsg_type = peer.address.ss_family == AF_INET ? IP_PKTINFO : IPV6_PKTINFO;
		header->cmsg_len = peer.address.ss_family == AF_INET ? CMSG_LEN(sizeof(in_pktinfo)) : CMSG_LEN(sizeof(in6_pktinfo));
		if (peer.address.ss_family == AF_INET)
			reinterpret_cast<in_pktinfo *>(CMSG_DATA(header))->ipi_ifindex = interface;
		else
			reinterpret_cast<in6_pktinfo *>(CMSG_DATA(header))->ipi6_ifindex = interface;
	}
	return sendmsg(fd, &outgoing, MSG_DONTWAIT);
}

void remove_network_handle(uint64_t handle)
{
	std::lock_guard lock(network_mutex);
	if (std::erase_if(network_handles, [handle](const auto & entry) { return entry.second == handle; }))
		++generation;
}

uint64_t network_generation()
{
	return generation.load();
}
} // namespace wivrn
