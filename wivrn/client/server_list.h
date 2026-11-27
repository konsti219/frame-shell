// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

#include "network.h"
#include "wivrn_discover.h"
#include <algorithm>
#include <cctype>
#include <limits>
#include <map>
#include <string>
#include <vector>

namespace wivrn
{
inline void prefer_low_latency(std::vector<wivrn_discover::service::address> & addresses)
{
	std::stable_sort(addresses.begin(), addresses.end(), [](const auto & a, const auto & b) {
		return a.latency_ms.value_or(std::numeric_limits<float>::infinity()) < b.latency_ms.value_or(std::numeric_limits<float>::infinity());
	});
}

struct server_list_link
{
	std::string interface;
	network_endpoint endpoint;
	float latency_ms;
};

inline std::vector<server_list_link> connected_server_links(const wivrn_discover::service & service)
{
	std::map<std::string, server_list_link> links;
	if (service.tcp_only)
		return {};
	for (const auto & address: service.addresses)
	{
		if (not address.latency_ms or address.interface.empty())
			continue;
		const auto endpoint = std::visit([](const auto & a) { return describe_endpoint(reinterpret_cast<const sockaddr *>(&a)); }, address.address);
		auto [it, inserted] = links.try_emplace(address.interface, server_list_link{address.interface, endpoint, *address.latency_ms});
		if (not inserted)
		{
			it->second.latency_ms = std::min(it->second.latency_ms, *address.latency_ms);
			if (std::holds_alternative<sockaddr_in>(address.address) and it->second.endpoint.address.contains(':'))
				it->second.endpoint = endpoint;
		}
	}
	std::vector<server_list_link> result;
	for (auto & [_, link]: links)
		result.push_back(std::move(link));
	return result;
}

inline bool same_server_endpoint(const wivrn_discover::service & manual, const wivrn_discover::service & discovered)
{
	if (manual.port != discovered.port)
		return false;
	const auto hostname = [](std::string name) {
		std::ranges::transform(name, name.begin(), [](unsigned char c) { return std::tolower(c); });
		while (name.ends_with('.'))
			name.pop_back();
		if (name.ends_with(".local"))
			name.resize(name.size() - 6);
		return name;
	};
	if (not manual.hostname.empty() and hostname(manual.hostname) == hostname(discovered.hostname))
		return true;
	for (const auto & address: discovered.addresses)
	{
		const auto endpoint = std::visit([](const auto & a) { return describe_endpoint(reinterpret_cast<const sockaddr *>(&a)); }, address.address);
		if (manual.hostname == endpoint.address and not endpoint.scope)
			return true;
		for (const auto & saved: manual.addresses)
		{
			const auto existing = std::visit([](const auto & a) { return describe_endpoint(reinterpret_cast<const sockaddr *>(&a)); }, saved.address);
			if (existing == endpoint and (not endpoint.scope or saved.interface == address.interface))
				return true;
		}
	}
	return false;
}

template <typename Servers>
std::string server_group_key(const Servers & servers, const wivrn_discover::service & service)
{
	const auto cookie = service.txt.find("cookie");
	const std::string id = cookie == service.txt.end() ? service.hostname : cookie->second;
	if (not service.manual)
		for (const auto & [key, saved]: servers)
			if (saved.manual and same_server_endpoint(saved.service, service))
				return key;
	return id;
}

template <typename Servers>
std::map<std::string, std::string> server_group_keys(const Servers & servers, const std::vector<wivrn_discover::service> & services)
{
	std::map<std::string, std::string> groups;
	for (const auto & service: services)
	{
		if (service.manual)
			continue;
		const auto cookie = service.txt.find("cookie");
		const std::string id = cookie == service.txt.end() ? service.hostname : cookie->second;
		const auto key = server_group_key(servers, service);
		auto [it, inserted] = groups.try_emplace(id, key);
		if (not inserted and key != id and (it->second == id or key < it->second))
			it->second = key;
	}
	return groups;
}
} // namespace wivrn
