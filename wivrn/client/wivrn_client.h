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

#include "crypto.h"
#include "multilink.h"
#include "network_poll.h"
#include "wivrn_packets.h"
#include "wivrn_sockets.h"
#include <chrono>
#include <poll.h>

using namespace wivrn;

class handshake_error : public std::exception
{
	std::string message;

public:
	const char * what() const noexcept override;
	handshake_error(std::string_view message);
};

class wivrn_session
{
public:
	using control_socket_t = typed_socket<TCP, to_headset::packets, from_headset::packets>;
	using stream_socket_t = typed_socket<multilink, to_headset::packets, from_headset::packets>;

private:
	control_socket_t control;
	stream_socket_t stream;
	bool multilink_enabled_;

	std::string server_id_;
	std::atomic<uint64_t> bytes_sent_ = 0;
	std::atomic<uint64_t> bytes_received_ = 0;

	template <typename T>
	void handshake(T address, bool tcp_only, crypto::key & headset_keypair, std::function<std::string(int fd)> pin_enter);

public:
	std::variant<in_addr, in6_addr> address;

	wivrn_session(sockaddr_in6 address, bool tcp_only, bool multilink_enabled, crypto::key & headset_keypair, std::function<std::string(int fd)> pin_enter);
	wivrn_session(sockaddr_in address, bool tcp_only, bool multilink_enabled, crypto::key & headset_keypair, std::function<std::string(int fd)> pin_enter);
	wivrn_session(const wivrn_session &) = delete;
	wivrn_session & operator=(const wivrn_session &) = delete;

	template <typename T>
	void send_control(T && packet)
	{
		if (stream)
			bytes_sent_ += stream.send_reliable(std::forward<T>(packet));
		else
			bytes_sent_ += control.send(std::forward<T>(packet));
	}

	template <typename T>
	void send_stream(T && packet)
	{
		if (stream)
			bytes_sent_ += stream.send(std::forward<T>(packet));
		else
			bytes_sent_ += control.send(std::forward<T>(packet));
	}

	template <typename T>
	void send_stream_reply(T && packet)
	{
		if (stream)
			bytes_sent_ += stream.send_reply(std::forward<T>(packet));
		else
			bytes_sent_ += control.send(std::forward<T>(packet));
	}

	template <typename T>
	int poll(T && visitor, std::chrono::milliseconds timeout)
	{
		pollfd fds[2] = {};
		fds[0].events = POLLIN;
		fds[0].fd = stream.get_fd();
		fds[1].events = POLLIN;
		fds[1].fd = stream ? -1 : control.get_fd();

		timeout = dispatch_pending_packets([&] { return stream.receive_pending(&bytes_received_); }, std::forward<T>(visitor), timeout);
		if (not stream)
			timeout = dispatch_pending_packets([&] { return control.receive_pending(&bytes_received_); }, std::forward<T>(visitor), timeout);

		int r = ::poll(fds, std::size(fds), timeout.count());
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

		if (fds[0].revents & POLLIN)
		{
			auto packet = stream.receive(&bytes_received_);
			if (packet)
			{
				std::visit(std::forward<T>(visitor), std::move(*packet));
			}
		}

		if (fds[1].revents & POLLIN)
		{
			auto packet = control.receive(&bytes_received_);
			if (packet)
				std::visit(std::forward<T>(visitor), std::move(*packet));
		}

		return r;
	}

	void shutdown()
	{
		::shutdown(control.get_fd(), SHUT_RDWR);
		stream.shutdown();
	}

	int control_fd()
	{
		return control.get_fd();
	}

	const std::string & server_id() const
	{
		return server_id_;
	}
	std::vector<link_statistics> links() const
	{
		return stream.statistics();
	}
	bool multilink_enabled() const
	{
		return multilink_enabled_ and bool(stream);
	}
	bool set_multilink_enabled(bool enabled)
	{
		if (not stream.set_multilink_enabled(enabled))
			return false;
		multilink_enabled_ = enabled;
		return true;
	}
	bool set_single_interface(std::string_view interface)
	{
		if (not stream.set_single_interface(interface))
			return false;
		multilink_enabled_ = false;
		return true;
	}
	multilink_diagnostics network_diagnostics() const
	{
		return stream.diagnostics();
	}

	uint64_t bytes_received() const
	{
		return bytes_received_;
	}

	uint64_t bytes_sent() const
	{
		return bytes_sent_;
	}
};
