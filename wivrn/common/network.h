// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

#include <cstdint>
#include <netinet/in.h>
#include <span>
#include <string>
#include <sys/socket.h>
#include <vector>

namespace wivrn
{
struct network_endpoint
{
	std::string address;
	std::string interface;
	uint16_t port = 0;
	uint32_t scope = 0;
	bool operator==(const network_endpoint &) const = default;
};

struct socket_address
{
	sockaddr_storage address{};
	socklen_t size = 0;
	bool operator==(const socket_address &) const;
};

socket_address parse_endpoint(const network_endpoint & endpoint);
network_endpoint describe_endpoint(const sockaddr * address, std::string interface = {});
std::vector<network_endpoint> local_endpoints(uint16_t port, bool include_loopback = false);
int open_network_socket(const network_endpoint & local);
ssize_t send_network_packet(int fd, const network_endpoint & local, const socket_address & peer, std::span<const uint8_t> data);
void set_network_handle(std::string interface, uint64_t handle);
void remove_network_handle(uint64_t handle);
uint64_t network_generation();
} // namespace wivrn
