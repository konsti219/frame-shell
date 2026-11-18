// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

#include "network.h"
#include <algorithm>
#include <array>
#include <atomic>
#include <cerrno>
#include <charconv>
#include <chrono>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <limits>
#include <map>
#include <memory>
#include <sstream>
#include <stdexcept>
#include <string_view>
#include <unistd.h>

namespace wivrn
{
// Test-only wire impairment. The caller still accounts accepted datagrams as sent.
class network_impairment
{
public:
	using clock = std::chrono::steady_clock;
	using time_point = clock::time_point;
	struct window
	{
		uint64_t offset_ms, duration_ms, delay_ms, jitter_ms, loss_per_mille, rate_bps;
	};
	struct route
	{
		size_t path_index, socket_index;
		uint64_t socket_identity;
		int fd;
		network_endpoint local;
		socket_address peer;
	};
	static constexpr size_t maximum_queue_bytes = 4 * 1024 * 1024;
	static constexpr auto maximum_residence = std::chrono::milliseconds(120);

private:
	struct counters
	{
		uint64_t offered = 0, offered_bytes = 0, carryover_offered = 0, forwarded = 0;
		uint64_t artificial_loss = 0, queue_overflow = 0, residence_drop = 0, send_fail = 0, stale_socket = 0;
		uint64_t delay_samples = 0, delay_sum_ns = 0, delay_min_ns = UINT64_MAX, delay_max_ns = 0;
		size_t queued_bytes = 0, peak_queued_bytes = 0, pending = 0;
		bool started = false, finished = false;
	};
	struct queued_packet
	{
		route destination;
		std::vector<uint8_t> wire;
		time_point enqueued;
		size_t window_index, cost;
	};
	struct path_tail
	{
		time_point due{};
		size_t window_index = 0;
	};
	std::vector<window> windows;
	std::vector<counters> counts;
	time_point start;
	std::filesystem::path log_directory;
	std::ofstream log;
	pid_t log_pid = 0;
	inline static std::atomic<uint64_t> next_instance = 0;
	uint64_t instance = next_instance.fetch_add(1);
	uint32_t random_state = 0x9e3779b9;
	std::multimap<time_point, queued_packet> queue;
	std::map<size_t, path_tail> tails;
	size_t queue_bytes = 0, queue_cost = 0, queue_limit;
	time_point rate_next{};
	size_t rate_window = SIZE_MAX;

	static int64_t ns(time_point t) { return std::chrono::duration_cast<std::chrono::nanoseconds>(t.time_since_epoch()).count(); }
	time_point begins(size_t i) const { return start + std::chrono::milliseconds(windows[i].offset_ms); }
	time_point ends(size_t i) const { return begins(i) + std::chrono::milliseconds(windows[i].duration_ms); }
	static uint64_t number(std::string_view value)
	{
		const auto first = value.find_first_not_of(" \t\r");
		if (first == value.npos)
			throw std::runtime_error("Empty network impairment CSV field");
		value = value.substr(first, value.find_last_not_of(" \t\r") - first + 1);
		uint64_t result = 0;
		const auto [end, error] = std::from_chars(value.data(), value.data() + value.size(), result);
		if (error != std::errc{} or end != value.data() + value.size())
			throw std::runtime_error("Invalid network impairment CSV integer");
		return result;
	}
	void ensure_log()
	{
		if (log_directory.empty() or log_pid == getpid())
			return;
		log.close();
		log.clear();
		log_pid = getpid();
		const auto file = log_directory / ("network-impairment-" + std::to_string(log_pid) + "-" + std::to_string(instance) + ".jsonl");
		log.open(file, std::ios::app);
		if (not log)
			throw std::runtime_error("Cannot open network impairment log");
		log << "{\"event\":\"profile\",\"pid\":" << log_pid << ",\"start_ns\":" << ns(start)
		    << ",\"windows\":" << windows.size() << ",\"queue_limit_bytes\":" << queue_limit
		    << ",\"maximum_residence_ms\":120,\"jitter\":\"triangle_10ms_80ms_period\",\"seed\":2654435769}\n";
		log.flush();
	}
	void log_windows(time_point now)
	{
		ensure_log();
		bool changed = false;
		for (size_t i = 0; i < windows.size(); ++i)
		{
			auto & c = counts[i];
			const auto & w = windows[i];
			if (not c.started and now >= begins(i))
			{
				c.started = true;
				changed = true;
				if (log)
					log << "{\"event\":\"window_start\",\"window\":" << i << ",\"start_ns\":" << ns(begins(i))
					    << ",\"end_ns\":" << ns(ends(i)) << ",\"observed_ns\":" << ns(now)
					    << ",\"delay_ms\":" << w.delay_ms << ",\"jitter_ms\":" << w.jitter_ms
					    << ",\"loss_per_mille\":" << w.loss_per_mille << ",\"rate_bps\":" << w.rate_bps << "}\n";
			}
			if (c.started and not c.finished and now >= ends(i) and c.pending == 0)
			{
				c.finished = true;
				changed = true;
				if (log)
					log << "{\"event\":\"window_end\",\"window\":" << i << ",\"start_ns\":" << ns(begins(i))
					    << ",\"end_ns\":" << ns(ends(i)) << ",\"observed_ns\":" << ns(now)
					    << ",\"offered\":" << c.offered << ",\"offered_bytes\":" << c.offered_bytes
					    << ",\"carryover_offered\":" << c.carryover_offered << ",\"forwarded\":" << c.forwarded
					    << ",\"artificial_loss\":" << c.artificial_loss << ",\"queue_overflow\":" << c.queue_overflow
					    << ",\"residence_drop\":" << c.residence_drop << ",\"send_fail\":" << c.send_fail
					    << ",\"stale_socket\":" << c.stale_socket << ",\"queued_bytes\":" << c.queued_bytes
					    << ",\"peak_queued_bytes\":" << c.peak_queued_bytes << ",\"delay_samples\":" << c.delay_samples
					    << ",\"delay_sum_ns\":" << c.delay_sum_ns << ",\"delay_min_ns\":" << (c.delay_samples ? c.delay_min_ns : 0)
					    << ",\"delay_max_ns\":" << c.delay_max_ns << "}\n";
			}
		}
		if (log and changed)
		{
			log.flush();
			if (not log)
				throw std::runtime_error("Writing network impairment log failed");
		}
	}
	void forwarded(counters & c, time_point now, time_point offered, ssize_t result)
	{
		if (result < 0)
		{
			++c.send_fail;
			c.stale_socket += errno == ESTALE;
			return;
		}
		++c.forwarded;
		const uint64_t delay = std::max<int64_t>(0, ns(now) - ns(offered));
		++c.delay_samples;
		c.delay_sum_ns += delay;
		c.delay_min_ns = std::min(c.delay_min_ns, delay);
		c.delay_max_ns = std::max(c.delay_max_ns, delay);
	}

public:
	network_impairment(time_point start, std::vector<window> windows, std::filesystem::path log_directory = {}, size_t queue_limit = maximum_queue_bytes) :
	        windows(std::move(windows)), counts(this->windows.size()), start(start), log_directory(std::move(log_directory)), queue_limit(queue_limit)
	{
		if (ns(start) <= 0 or this->windows.empty() or this->windows.size() > 64 or not queue_limit or queue_limit > maximum_queue_bytes)
			throw std::runtime_error("Invalid network impairment profile bounds");
		uint64_t previous_end = 0;
		for (const auto & w: this->windows)
		{
			if (w.offset_ms < previous_end or w.offset_ms > 86'400'000 or not w.duration_ms or w.duration_ms > 60'000
			    or w.delay_ms > 120 or w.jitter_ms > 120 - w.delay_ms or w.loss_per_mille > 1000
			    or (w.rate_bps and (w.rate_bps < 64'000 or w.rate_bps > 10'000'000'000ULL))
			    or w.offset_ms + w.duration_ms > uint64_t((INT64_MAX - ns(start)) / 1'000'000))
				throw std::runtime_error("Invalid or overlapping network impairment window");
			previous_end = w.offset_ms + w.duration_ms;
		}
		if (not this->log_directory.empty())
		{
			if (not this->log_directory.is_absolute())
				throw std::runtime_error("Network impairment log directory must be absolute");
			std::filesystem::create_directories(this->log_directory);
			ensure_log();
		}
	}
	static std::unique_ptr<network_impairment> from_environment()
	{
		const char * file = std::getenv("WIVRN_TEST_NETWORK_PROFILE");
		if (not file)
			return {};
		const std::filesystem::path path(file);
		if (not path.is_absolute())
			throw std::runtime_error("WIVRN_TEST_NETWORK_PROFILE must be an absolute CSV path");
		std::ifstream input(path);
		std::string line;
		if (not input or not std::getline(input, line))
			throw std::runtime_error("Cannot read network impairment profile");
		const auto start_ns = number(line);
		if (start_ns > INT64_MAX)
			throw std::runtime_error("Network impairment start exceeds CLOCK_MONOTONIC range");
		std::vector<window> windows;
		while (std::getline(input, line))
		{
			if (line.find_first_not_of(" \t\r") == line.npos)
				continue;
			std::array<uint64_t, 6> fields{};
			std::string_view remaining(line);
			for (size_t i = 0; i < fields.size(); ++i)
			{
				const auto comma = remaining.find(',');
				if ((i + 1 == fields.size()) != (comma == remaining.npos))
					throw std::runtime_error("Network impairment rows require six CSV integers");
				fields[i] = number(remaining.substr(0, comma));
				if (comma != remaining.npos)
					remaining.remove_prefix(comma + 1);
			}
			windows.push_back({fields[0], fields[1], fields[2], fields[3], fields[4], fields[5]});
			if (windows.size() > 64)
				throw std::runtime_error("Too many network impairment windows");
		}
		if (input.bad())
			throw std::runtime_error("Reading network impairment profile failed");
		const char * directory = std::getenv("WIVRN_TEST_NETWORK_LOG");
		if (directory and not *directory)
			throw std::runtime_error("WIVRN_TEST_NETWORK_LOG must be an absolute directory");
		return std::make_unique<network_impairment>(time_point(std::chrono::nanoseconds(start_ns)), std::move(windows), directory ? std::filesystem::path(directory) : path.parent_path());
	}
	size_t queued_bytes() const { return queue_bytes; }
	size_t charged_queue_bytes() const { return queue_cost; }
	void controller_sample(time_point now, size_t path, bool active, uint64_t before, uint64_t after,
	                       uint64_t sent, uint64_t lost, uint64_t acknowledged, double observed,
	                       double rtt, double queue_delay, double latest_queue_delay,
	                       double ack_age_max_ms, const char * action)
	{
		ensure_log();
		if (not log)
			return;
		log << "{\"event\":\"controller\",\"now_ns\":" << ns(now) << ",\"path\":" << path
		    << ",\"active\":" << active << ",\"capacity_before\":" << before << ",\"capacity_after\":" << after
		    << ",\"sent\":" << sent << ",\"lost\":" << lost << ",\"acknowledged_bytes\":" << acknowledged
		    << ",\"observed_bps\":" << observed << ",\"rtt_ms\":" << rtt
		    << ",\"queue_delay_ms\":" << queue_delay << ",\"latest_queue_delay_ms\":" << latest_queue_delay
		    << ",\"ack_age_max_ms\":" << ack_age_max_ms
		    << ",\"action\":\"" << action << "\"}\n";
		log.flush();
	}

	template <typename Sender>
	ssize_t send(time_point now, const route & destination, std::span<const uint8_t> wire, Sender && sender)
	{
		pump(now, sender);
		now = std::max(now, clock::now());
		size_t owner = SIZE_MAX;
		for (size_t i = 0; i < windows.size(); ++i)
			if (now >= begins(i) and now < ends(i))
			{
				owner = i;
				break;
			}
		const auto tail = tails.find(destination.path_index);
		const bool carryover = owner == SIZE_MAX and tail != tails.end() and tail->second.due > now;
		if (owner == SIZE_MAX and not carryover)
			return sender(destination, wire);
		if (carryover)
			owner = tail->second.window_index;
		auto & c = counts[owner];
		++c.offered;
		c.offered_bytes += wire.size();
		c.carryover_offered += carryover;
		const auto & w = windows[owner];
		random_state ^= random_state << 13;
		random_state ^= random_state >> 17;
		random_state ^= random_state << 5;
		if (not carryover and random_state % 1000 < w.loss_per_mille)
		{
			++c.artificial_loss;
			return wire.size();
		}
		auto due = now;
		auto next_rate = rate_next;
		if (not carryover)
		{
			if (w.rate_bps)
			{
				if (rate_window != owner)
					next_rate = now;
				next_rate = std::max(now, next_rate) + std::chrono::nanoseconds((wire.size() + 28) * 8'000'000'000ULL / w.rate_bps);
				due = next_rate;
			}
			const auto bucket = std::chrono::duration_cast<std::chrono::milliseconds>(now - begins(owner)).count() / 10 % 8;
			const auto triangle = bucket <= 4 ? bucket : 8 - bucket;
			due += std::chrono::microseconds(w.delay_ms * 1000 + w.jitter_ms * 1000 * triangle / 4);
		}
		if (tail != tails.end())
			due = std::max(due, tail->second.due);
		if (due <= now)
		{
			const auto result = sender(destination, wire);
			forwarded(c, now, now, result);
			return result;
		}
		const size_t cost = wire.size() + sizeof(queued_packet) + 64 + destination.local.address.size() + destination.local.interface.size();
		if (due - now >= maximum_residence or cost > queue_limit - std::min(queue_limit, queue_cost))
		{
			++c.queue_overflow;
			return wire.size();
		}
		queue.emplace(due, queued_packet{destination, std::vector<uint8_t>(wire.begin(), wire.end()), now, owner, cost});
		tails.insert_or_assign(destination.path_index, path_tail{due, owner});
		if (not carryover and w.rate_bps)
		{
			rate_next = next_rate;
			rate_window = owner;
		}
		queue_cost += cost;
		queue_bytes += wire.size();
		c.queued_bytes += wire.size();
		c.peak_queued_bytes = std::max(c.peak_queued_bytes, c.queued_bytes);
		++c.pending;
		return wire.size();
	}

	template <typename Sender>
	void pump(time_point now, Sender && sender)
	{
		while (not queue.empty() and queue.begin()->first <= now)
		{
			auto it = queue.begin();
			const auto & p = it->second;
			auto & c = counts[p.window_index];
			const auto dispatched = std::max(now, clock::now());
			if (dispatched - p.enqueued >= maximum_residence)
				++c.residence_drop;
			else
			{
				const auto result = sender(p.destination, p.wire);
				forwarded(c, std::max(dispatched, clock::now()), p.enqueued, result);
			}
			queue_bytes -= p.wire.size();
			queue_cost -= p.cost;
			c.queued_bytes -= p.wire.size();
			--c.pending;
			queue.erase(it);
		}
		log_windows(std::max(now, clock::now()));
	}
};
} // namespace wivrn
