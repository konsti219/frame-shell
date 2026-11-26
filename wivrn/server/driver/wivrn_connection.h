/*
 * WiVRn VR streaming
 * Copyright (C) 2022  Guillaume Meunier <guillaume.meunier@centraliens.net>
 * Copyright (C) 2022  Patrick Nicolas <patricknicolas@laposte.net>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#pragma once

#include "multilink.h"
#include "wivrn_ipc.h"
#include "wivrn_packets.h"
#include "wivrn_sockets.h"

#include <atomic>
#include <optional>
#include <poll.h>
#include <stdexcept>
#include <stop_token>
#include <system_error>

namespace wivrn
{
class incorrect_pin : public std::runtime_error
{
public:
	incorrect_pin();
};

class wivrn_connection
{
public:
	enum class encryption_state
	{
		disabled,
		enabled,
		pairing,
	};

private:
	typed_socket<TCP, from_headset::packets, to_headset::packets> control;
	typed_socket<multilink, from_headset::packets, to_headset::packets> stream;
	std::atomic<bool> active = false;
	std::chrono::steady_clock::time_point last_received{};
	std::string pin;
	encryption_state state;

	from_headset::headset_info_packet info_packet;

	void init(std::stop_token stop_token, std::function<void()> tick = []() {});

public:
	wivrn_connection(std::stop_token stop_token, encryption_state state, std::string pin, TCP && tcp);
	wivrn_connection(const wivrn_connection &) = delete;
	wivrn_connection & operator=(const wivrn_connection &) = delete;

	bool has_stream() const
	{
		return bool(stream);
	}

	uint64_t capacity_bps() const
	{
		return stream.capacity_bps();
	}

	bool is_active()
	{
		return active;
	}
	void reset(std::stop_token stop, TCP && tcp, std::function<void()> tick = {});
	void shutdown();
	void prepare_fork()
	{
		stream.prepare_fork();
	}
	void resume_after_fork()
	{
		stream.resume_after_fork();
	}

	template <typename T>
	void send_control(T && packet)
	{
		try
		{
			if (active)
			{
				if (stream)
				{
					if constexpr (std::is_same_v<std::decay_t<T>, to_headset::video_shard>)
						stream.send_protected_video(std::forward<T>(packet), std::chrono::steady_clock::now() + std::chrono::milliseconds(25));
					else if constexpr (std::is_same_v<std::decay_t<T>, to_headset::application_icon>)
						stream.send_bulk(std::forward<T>(packet));
					else
						stream.send_reliable(std::forward<T>(packet));
				}
				else
					control.send(std::forward<T>(packet));
			}
		}
		catch (...)
		{
			active = false;
			throw;
		}
	}

	to_headset::network_report network_statistics() const
	{
		to_headset::network_report result{.multilink = bool(stream)};
		const auto queued = stream.diagnostics();
		result.expired_video = queued.expired_video;
		result.queued_bytes = queued.queued_bytes;
		result.reliable_messages = queued.reliable_messages;
		result.bulk_messages = queued.bulk_messages;
		result.protected_video_messages = queued.protected_video_messages;
		result.rejected_video = queued.rejected_video;
		result.oldest_control_ms = queued.oldest_control_ms;
		for (const auto & link: stream.statistics())
		{
			result.repairs_sent += link.repairs_sent;
			if (link.ever_active)
				result.links.push_back({link.local.interface, link.remote.interface, link.capacity_bps, link.sent_bps, float(link.loss)});
		}
		return result;
	}

	template <typename T>
	void send_stream(T && packet, std::chrono::steady_clock::time_point deadline = {}, bool protect = false)
	{
		try
		{
			if (active)
			{
				if (stream)
				{
					if constexpr (std::is_same_v<std::decay_t<T>, to_headset::video_shard>)
						stream.send_video(std::forward<T>(packet), deadline == std::chrono::steady_clock::time_point{} ? std::chrono::steady_clock::now() + std::chrono::milliseconds(packet.stream < to_headset::video_shard::desktop_streams ? 25 : 100) : deadline, protect);
					else
						stream.send(std::forward<T>(packet));
				}
				else
					control.send(std::forward<T>(packet));
			}
		}
		catch (...)
		{
			active = false;
			throw;
		}
	}

	std::optional<from_headset::packets> poll_control(int timeout);

	const from_headset::headset_info_packet & info()
	{
		return info_packet;
	}

	template <typename T>
	int poll(T && visitor, int timeout)
	{
		pollfd fds[3] = {};
		fds[0].events = POLLIN;
		fds[0].fd = stream.get_fd();
		fds[1].events = POLLIN;
		fds[1].fd = stream ? -1 : control.get_fd();
		fds[2].fd = wivrn_ipc_socket_monado->get_fd();
		fds[2].events = POLLIN;

		while (auto packet = stream.receive_pending())
		{
			last_received = std::chrono::steady_clock::now();
			std::visit(std::forward<T>(visitor), std::move(*packet));
		}
		while (auto packet = stream ? std::nullopt : control.receive_pending())
		{
			if (not stream)
				last_received = std::chrono::steady_clock::now();
			std::visit(std::forward<T>(visitor), std::move(*packet));
		}

		int r = ::poll(fds, std::size(fds), timeout);
		if (r < 0)
			throw std::system_error(errno, std::system_category());

		// UDP reports ICMP errors (e.g. a stray port-unreachable): transient, a dead path shows as silence instead
		if (fds[0].revents & POLLERR)
		{
			int error = 0;
			socklen_t size = sizeof(error);
			getsockopt(stream.get_fd(), SOL_SOCKET, SO_ERROR, &error, &size);
		}
		if (fds[0].revents & POLLHUP)
			throw std::runtime_error("Error on stream socket");

		if (fds[1].revents & (POLLHUP | POLLERR))
			throw std::runtime_error("Error on control socket");

		if (fds[2].revents & (POLLHUP | POLLERR))
			throw std::runtime_error("Error on IPC socket");

		if (fds[0].revents & POLLIN)
		{
			auto packet = stream.receive();
			if (packet)
				last_received = std::chrono::steady_clock::now();
			if (packet)
				std::visit(std::forward<T>(visitor), std::move(*packet));
		}

		if (fds[1].revents & POLLIN)
		{
			auto packet = control.receive();
			if (packet and not stream)
				last_received = std::chrono::steady_clock::now();
			if (packet)
				std::visit(std::forward<T>(visitor), std::move(*packet));
		}

		if (fds[2].revents & POLLIN)
		{
			auto packet = receive_from_main();
			if (packet)
				std::visit(std::forward<T>(visitor), std::move(*packet));
		}
		if (std::chrono::steady_clock::now() - last_received > std::chrono::seconds(1))
			throw std::runtime_error("Nothing received from the headset for a second");
		return r;
	}
};
} // namespace wivrn
