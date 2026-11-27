#pragma once

#include "wifi_lock.h"

#include <chrono>
#include <map>
#include <memory>
#include <mutex>
#include <netinet/in.h>
#include <optional>
#include <string>
#include <thread>
#include <variant>
#include <vector>

class dnssd_cache;

class wivrn_discover
{
public:
	struct service
	{
		struct address
		{
			std::string interface;
			std::variant<sockaddr_in, sockaddr_in6> address;
			std::optional<float> latency_ms;
		};
		std::string name;
		std::string hostname;
		int port;
		bool tcp_only = false;
		bool manual = false;

		std::vector<address> addresses;
		std::map<std::string, std::string> txt;
		std::chrono::steady_clock::time_point ttl;
		std::string pin;
	};

	static inline const std::chrono::milliseconds poll_min_time{500};
	static inline const std::chrono::milliseconds poll_max_time{10000};
	static inline const std::chrono::milliseconds discover_period{5000};

private:
	wifi_lock::multicast multicast;
	std::unique_ptr<dnssd_cache> cache;

	std::mutex mutex;
	std::vector<service> services;
	std::vector<service> manual_services;
	std::atomic<bool> quit = false;
	std::thread dnssd_thread;

	void discover(std::string service_name);

public:
	wivrn_discover(std::string service_name = "_wivrn._tcp.local.");
	wivrn_discover(const wivrn_discover &) = delete;
	wivrn_discover(wivrn_discover &&) = delete;
	wivrn_discover operator=(const wivrn_discover &) = delete;
	wivrn_discover operator=(wivrn_discover &&) = delete;
	~wivrn_discover();

	std::vector<service> get_services();
	void set_manual_services(std::vector<service> services);
};
