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

#include "wivrn_client.h"
#include "application.h"
#include "protocol_version.h"
#include "secrets.h"
#include "smp.h"
#include "spdlog/common.h"
#include "utils/i18n.h"
#include "wivrn_packets.h"
#include <arpa/inet.h>
#include <ifaddrs.h>
#include <linux/ipv6.h>
#include <net/if.h>
#include <netinet/ip.h>
#include <poll.h>
#include <spdlog/spdlog.h>
#include <stdexcept>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef IPTOS_DSCP_EF
// constant is not defined in Android ip.h
#define IPTOS_DSCP_EF 0xb8
#endif

using namespace std::chrono_literals;

const char * handshake_error::what() const noexcept
{
	return message.c_str();
}

handshake_error::handshake_error(std::string_view message) :
        message(message) {}

template <typename T>
void wivrn_session::handshake(T address, bool tcp_only, crypto::key & headset_keypair, std::function<std::string(int fd)> pin_enter)
{
	pollfd fds{};
	fds.events = POLLIN;
	fds.fd = control.get_fd();

	auto receive = [&](std::optional<std::chrono::seconds> timeout = std::nullopt) {
		std::chrono::steady_clock::time_point timeout_abs{};
		if (timeout)
			timeout_abs = std::chrono::steady_clock::now() + *timeout;

		while (true)
		{
			auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(timeout_abs - std::chrono::steady_clock::now());
			int r = ::poll(&fds, 1, std::max<int>(ms.count(), 100));
			if (r < 0)
				throw std::system_error(errno, std::system_category());

			if (r > 0 && (fds.revents & POLLIN))
			{
				auto packet = control.receive();
				if (not packet)
					continue;

				return std::move(*packet);
			}

			if (std::chrono::steady_clock::now() >= timeout_abs)
				throw std::runtime_error(_("Timeout"));
		}
	};

	send_control(from_headset::crypto_handshake{
	        .protocol_version = wivrn::protocol_version,
	        .public_key = headset_keypair.public_key(),
	        .name = application::get_hmd_traits().model_name(),
	});

	to_headset::crypto_handshake crypto_handshake = std::get<to_headset::crypto_handshake>(receive(10s));

	auto setup_stream = [&](const to_headset::handshake & offer) {
		server_id_ = offer.server_id;
		if (offer.stream_port <= 0 or tcp_only)
			return;
		auto primary = describe_endpoint(reinterpret_cast<const sockaddr *>(&address));
		auto local = local_endpoints(0, primary.address.starts_with("127.") or primary.address == "::1");
		auto remote = offer.endpoints;
		primary.port = offer.stream_port;
		if (std::ranges::none_of(remote, [&](const auto & endpoint) { return endpoint.address == primary.address; }))
			remote.push_back(primary);
		if (not multilink_enabled_)
		{
			sockaddr_storage source{};
			socklen_t size = sizeof(source);
			if (getsockname(control.get_fd(), reinterpret_cast<sockaddr *>(&source), &size) != 0)
				throw std::system_error(errno, std::generic_category(), "Reading connection address");
			const auto selected = describe_endpoint(reinterpret_cast<const sockaddr *>(&source));
			const auto endpoint = std::ranges::find_if(local, [&](const auto & candidate) {
				return candidate.address == selected.address and (not selected.scope or candidate.scope == selected.scope);
			});
			if (endpoint == local.end())
				throw std::runtime_error(_("The selected network interface is unavailable"));
			local = {*endpoint};
			remote = {primary};
		}
		stream = stream_socket_t(false, offer.multilink_key, std::move(local), std::move(remote), multilink_enabled_, multilink_enabled_);
	};

	std::string pin = "000000";
	switch (crypto_handshake.state)
	{
		case to_headset::crypto_handshake::crypto_state::encryption_disabled: {
			spdlog::info("Encryption is disabled on server");

			send_control(from_headset::crypto_handshake{});

			to_headset::handshake h{std::get<to_headset::handshake>(receive(10s))};
			setup_stream(h);
			break;
		}

		case to_headset::crypto_handshake::crypto_state::pin_needed:
			pin = pin_enter(control.get_fd());

			// Check the PIN
			try
			{
				crypto::smp pin_check;

				auto msg1 = pin_check.step1(pin);
				send_control(from_headset::pin_check_1{msg1});

				auto msg2 = std::get<to_headset::pin_check_2>(receive(10s)).message;

				auto msg3 = pin_check.step3(msg2);
				send_control(from_headset::pin_check_3{msg3});

				auto msg4 = std::get<to_headset::pin_check_4>(receive(10s)).message;
				bool pin_match = pin_check.step5(msg4);

				if (not pin_match)
					throw std::runtime_error(_("Incorrect PIN"));
			}
			catch (crypto::smp_cheated &)
			{
				throw std::runtime_error(_("Unable to check PIN"));
			}

			[[fallthrough]];

		case to_headset::crypto_handshake::crypto_state::client_already_paired: {
			spdlog::info("Using pin \"{}\"", pin);

			crypto::key server_key = crypto::key::from_public_key(crypto_handshake.public_key);
			secrets s{headset_keypair, server_key, pin};
			control.set_aes_key_and_ivs(s.control_key, s.control_iv_to_headset, s.control_iv_from_headset);

			// Confirm that encryption is set up
			send_control(from_headset::crypto_handshake{});

			to_headset::handshake h{std::get<to_headset::handshake>(receive(10s))};
			setup_stream(h);
			break;
		}

		case to_headset::crypto_handshake::crypto_state::pairing_disabled:
			spdlog::info("Pairing is disabled on server");
			throw std::runtime_error(_("Pairing is disabled on server"));

		case to_headset::crypto_handshake::crypto_state::incompatible_version:
			spdlog::error("Incompatible protocol versions");
			throw std::runtime_error(_("Incompatible server version"));
	}

	// may be on control socket if forced TCP
	if (stream)
		stream.send_reliable(from_headset::handshake{});
	else
		control.send(from_headset::handshake{});

	// Complete bootstrap on TCP. Subsequent control messages use the bonded channel.
	(void)std::get<to_headset::handshake>(receive(10s));
}

wivrn_session::wivrn_session(sockaddr_in6 address, bool tcp_only, bool multilink_enabled, crypto::key & headset_keypair, std::function<std::string(int fd)> pin_enter) :
        control(address), stream(), multilink_enabled_(multilink_enabled), address(address.sin6_addr)
{
	try
	{
		handshake(address, tcp_only, headset_keypair, pin_enter);
	}
	catch (std::exception & e)
	{
		throw handshake_error{e.what()};
	}
}

wivrn_session::wivrn_session(sockaddr_in address, bool tcp_only, bool multilink_enabled, crypto::key & headset_keypair, std::function<std::string(int fd)> pin_enter) :
        control(address), stream(), multilink_enabled_(multilink_enabled), address(address.sin_addr)
{
	try
	{
		handshake(address, tcp_only, headset_keypair, pin_enter);
	}
	catch (std::exception & e)
	{
		throw handshake_error{e.what()};
	}
}
