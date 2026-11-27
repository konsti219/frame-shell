// SPDX-License-Identifier: GPL-3.0-or-later
#include "../client/network_interface.h"
#include "../client/server_list.h"
#include <cstdlib>
#include <iostream>
#include <source_location>

using namespace wivrn;
static void check(bool condition, std::source_location at = std::source_location::current())
{
	if (not condition)
	{
		std::cerr << at.file_name() << ':' << at.line() << '\n';
		std::abort();
	}
}
static wivrn_discover::service::address link(std::string interface, std::string ip, std::optional<float> latency)
{
	const auto address = parse_endpoint({ip, interface, 9757});
	if (address.address.ss_family == AF_INET)
		return {interface, reinterpret_cast<const sockaddr_in &>(address.address), latency};
	return {interface, reinterpret_cast<const sockaddr_in6 &>(address.address), latency};
}
int main()
{
	{
		std::vector<wivrn_discover::service::address> addresses{
		        link("wlan0", "10.26.0.56", 8),
		        link("usb0", "10.86.200.1", {}),
		        link("wlanap", "10.35.78.30", 2),
		};
		prefer_low_latency(addresses);
		check(addresses[0].interface == "wlanap");
		check(addresses[1].interface == "wlan0");
		check(addresses[2].interface == "usb0");
	}
	wivrn_discover::service discovered{.name = "Polaris", .hostname = "polaris", .port = 9757, .addresses = {link("wlan0", "10.26.0.56", 9.2f), link("wlanap", "10.35.78.30", 94.2f), link("usb0", "10.86.200.1", {}), link("wlan0", "fd00::56", 8.0f)}, .txt = {{"cookie", "server-id"}}};
	const auto links = connected_server_links(discovered);
	check(links.size() == 2);
	check(links[0].interface == "wlan0" and links[0].endpoint.address == "10.26.0.56" and links[0].latency_ms == 8.0f);
	check(links[1].interface == "wlanap" and links[1].endpoint.address == "10.35.78.30");
	struct saved_server
	{
		bool manual;
		wivrn_discover::service service;
	};
	std::map<std::string, saved_server> servers{{"manual-polaris", {true, {.hostname = "10.35.78.30", .port = 9757}}}};
	check(server_group_key(servers, discovered) == "manual-polaris");
	auto lan = discovered, direct = discovered;
	lan.addresses = {link("wlan0", "10.26.0.56", 9.2f)};
	direct.addresses = {link("wlanap", "10.35.78.30", 94.2f)};
	check(server_group_keys(servers, {lan, direct}).at("server-id") == "manual-polaris");
	check(server_group_keys(servers, {direct, lan}).at("server-id") == "manual-polaris");
	servers.at("manual-polaris").service.hostname = "POLARIS.local.";
	check(server_group_key(servers, discovered) == "manual-polaris");
	servers.at("manual-polaris").service.port = 9758;
	check(server_group_key(servers, discovered) == "server-id");
	servers.at("manual-polaris").service = {.name = "Polaris", .hostname = "other-host", .port = 9757};
	check(server_group_key(servers, discovered) == "server-id");
	discovered.addresses = {link("wlan0", "10.26.0.56", {}), link("usb0", "10.86.200.1", {})};
	check(connected_server_links(discovered).empty());
	discovered.addresses = {link("wlan0", "fd00::56", 4.0f)};
	check(connected_server_links(discovered).front().endpoint.address == "fd00::56");
	discovered.tcp_only = true;
	check(connected_server_links(discovered).empty());
	check(classify_interface("wlan0", true, true) == interface_type::wifi);
	check(classify_interface("wlanap", true, true) == interface_type::dongle);
	check(classify_interface("wlanap", false, true) == interface_type::wifi);
	check(classify_interface("usb0", true, false) == interface_type::usb);
	std::cout << "Server grouping, reachable links, address alternatives and Frame labels passed\n";
}
