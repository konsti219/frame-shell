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

#include "wivrn_connection.h"
#include "configuration.h"
#include "protocol_version.h"
#include "secrets.h"
#include "smp.h"
#include "utils/wivrn_trace.h"
#include "wivrn_ipc.h"
#include "wivrn_packets.h"
#include <algorithm>
#include <arpa/inet.h>
#include <chrono>
#include <cstdlib>
#include <poll.h>
#include <regex>
#include <sys/socket.h>
#include <variant>

using namespace std::chrono_literals;

// Ignored until connection is established
static void handle_event_from_main_loop(to_monado::stop) {}
static void handle_event_from_main_loop(to_monado::disconnect) {}
static void handle_event_from_main_loop(to_monado::set_bitrate) {}
static void handle_event_from_main_loop(wivrn::to_headset::stream_tab_change) {}

static std::string clean_key(std::string key)
{
	static const std::regex header{"^-+BEGIN .*-+$", std::regex_constants::multiline};
	static const std::regex footer{"^-+END .*-+$", std::regex_constants::multiline};
	static const std::regex whitespace{"[[:space:]]"};

	key = std::regex_replace(key, header, "");
	key = std::regex_replace(key, footer, "");
	key = std::regex_replace(key, whitespace, "");

	return key;
}

wivrn::incorrect_pin::incorrect_pin() :
        std::runtime_error("Incorrect PIN") {}

wivrn::wivrn_connection::wivrn_connection(std::stop_token stop_token, encryption_state state, std::string pin, TCP && tcp) :
        control(std::move(tcp)),
        stream(),
        pin(pin),
        state(state)
{
	init(stop_token);
}

void wivrn::wivrn_connection::init(std::stop_token stop_token, std::function<void()> tick)
{
	active = false;

	sockaddr_in6 server_address;
	socklen_t len = sizeof(server_address);
	if (getsockname(control.get_fd(), (sockaddr *)&server_address, &len) < 0)
	{
		throw std::system_error(errno, std::system_category(), "Cannot get socket port");
	}
	int port = ntohs(((struct sockaddr_in6 *)&server_address)->sin6_port);

	sockaddr_in6 client_address;
	len = sizeof(client_address);
	if (getpeername(control.get_fd(), (sockaddr *)&client_address, &len) < 0)
	{
		throw std::system_error(errno, std::system_category(), "Cannot get client address");
	}

	const auto multilink_key = multilink::random_key();
	const configuration config;
	if (config.tcp_only)
	{
		port = -1;
	}

	auto receive = [&](std::optional<std::chrono::seconds> timeout = std::nullopt, bool allow_stream_socket = false) {
		// Returns the packet and the port (on the stream socket) or -1 (on the control socket)

		std::chrono::steady_clock::time_point timeout_abs{};
		if (timeout)
			timeout_abs = std::chrono::steady_clock::now() + *timeout;

		while (true)
		{
			if (stop_token.stop_requested())
				throw std::runtime_error("Connection cancelled");

			tick();

			pollfd fds[3] = {};
			if (allow_stream_socket)
			{
				fds[0].events = POLLIN;
				fds[0].fd = stream.get_fd();
			}
			fds[1].events = POLLIN;
			fds[1].fd = control.get_fd();
			fds[2].events = POLLIN;
			fds[2].fd = wivrn_ipc_socket_monado->get_fd();

			// Make sure tick() is called at least every 100ms
			int r = ::poll(fds, std::size(fds), 100);
			if (r < 0)
				throw std::system_error(errno, std::system_category());

			if (allow_stream_socket and (fds[0].revents & (POLLHUP | POLLERR)))
				throw std::runtime_error("Error on stream socket");

			if (fds[1].revents & (POLLHUP | POLLERR))
				throw std::runtime_error("Error on control socket");

			if (fds[2].revents & (POLLHUP | POLLERR))
				throw std::runtime_error("Error on IPC socket");

			if (allow_stream_socket and (fds[0].revents & POLLIN))
			{
				if (auto packet = stream.receive())
					return std::make_pair(std::move(*packet), 0);
			}

			if (fds[1].revents & POLLIN)
			{
				std::optional<from_headset::packets> packet = control.receive();
				if (packet)
					return std::make_pair(std::move(*packet), -1);
			}

			if (fds[2].revents & POLLIN)
			{
				auto packet = receive_from_main();
				if (packet)
					std::visit([](auto && x) { handle_event_from_main_loop(x); }, *packet);
			}

			if (timeout and std::chrono::steady_clock::now() > timeout_abs)
			{
				throw std::runtime_error("No handshake received from client");
			}
		}
	};

	// Wait for client to send handshake packet
	auto crypto_handshake = std::get<from_headset::crypto_handshake>(receive(10s).first);

	if (crypto_handshake.protocol_version != wivrn::protocol_version)
	{
		control.send(to_headset::crypto_handshake{
		        .state = to_headset::crypto_handshake::crypto_state::incompatible_version,
		});
		throw std::runtime_error("Incompatible protocol version");
	}

	crypto::key headset_key = crypto::key::from_public_key(crypto_handshake.public_key);
	bool is_public_key_known = std::ranges::any_of(
	        wivrn::known_keys(),
	        [key = clean_key(crypto_handshake.public_key)](const wivrn::headset_key & k) {
		        return k.public_key == key;
	        });

	switch (state)
	{
		case encryption_state::disabled:
			// Encryption and authentication are disabled
			control.send(to_headset::crypto_handshake{
			        .state = to_headset::crypto_handshake::crypto_state::encryption_disabled,
			});
			break;

		case encryption_state::enabled:
			if (not is_public_key_known)
			{
				control.send(to_headset::crypto_handshake{
				        .state = to_headset::crypto_handshake::crypto_state::pairing_disabled,
				});
				throw std::runtime_error("Client not known and pairing is disabled");
			}

			[[fallthrough]];

		case encryption_state::pairing:
			// Generate an ephemeral key pair just for exchanging the AES key
			crypto::key server_key = crypto::key::generate_x448_keypair();

			control.send(to_headset::crypto_handshake{
			        .public_key = server_key.public_key(),
			        .state = is_public_key_known
			                         ? to_headset::crypto_handshake::crypto_state::client_already_paired
			                         : to_headset::crypto_handshake::crypto_state::pin_needed,
			});

			if (not is_public_key_known)
			{
				try
				{
					// Check the PIN
					crypto::smp pin_check;

					auto msg1 = std::get<from_headset::pin_check_1>(receive(2min).first).message;

					auto msg2 = pin_check.step2(msg1, pin);
					control.send(to_headset::pin_check_2{msg2});

					auto msg3 = std::get<from_headset::pin_check_3>(receive(10s).first).message;

					auto [msg4, pin_match] = pin_check.step4(msg3);
					control.send(to_headset::pin_check_4{msg4});

					if (not pin_match)
						throw incorrect_pin{};
				}
				catch (crypto::smp_cheated &)
				{
					throw std::runtime_error("Unable to check PIN");
				}
			}

			secrets s{server_key, headset_key, is_public_key_known ? "000000" : pin};
			control.set_aes_key_and_ivs(s.control_key, s.control_iv_from_headset, s.control_iv_to_headset);
			break;
	}

	// Wait for confirmation that the client has set up encryption
	if (not std::holds_alternative<from_headset::crypto_handshake>(receive().first))
		throw std::runtime_error("No handshake received from client");

	if (port > 0)
	{
		auto primary = describe_endpoint(reinterpret_cast<const sockaddr *>(&server_address));
		auto local = local_endpoints(port, primary.address.starts_with("127.") or primary.address == "::1");
		stream = decltype(stream)(true, multilink_key, std::move(local));
		stream.set_options({.fec_group_size = uint8_t(config.multilink_fec ? 4 : 0), .aggregate_limit_bps = config.multilink_rate_limit, .shared_bottleneck = config.multilink_shared_bottleneck});
		// The runtime initializes tracing after establishing this connection.
		if (std::getenv("WIVRN_TRACING"))
			stream.set_video_queue_drop_observer([](const video_queue_drop & drop) {
				if (not trace::is_enabled())
					return;
				std::array<uint8_t, 1 + 2 * sizeof(uint32_t) + 2 * sizeof(uint64_t) + sizeof(uint16_t)> header;
				if (drop.packet.size() < header.size())
					return;
				std::copy_n(drop.packet.begin(), header.size(), header.begin());
				deserialization_packet packet({}, header);
				packet.deserialize<uint8_t>();
				const auto stream = packet.deserialize<uint32_t>();
				const auto generation = packet.deserialize<uint32_t>();
				const auto frame = packet.deserialize<uint64_t>();
				packet.deserialize<uint64_t>();
				const auto shard = packet.deserialize<uint16_t>();
				trace::video_queue_dropped(drop, stream, generation, frame, shard);
			});
	}

	control.send(to_headset::handshake{.stream_port = port, .multilink_key = multilink_key, .endpoints = stream.endpoints(), .server_id = server_cookie()});

	auto [stream_handshake, client_port] = receive(10s, true);

	// Check the packet type
	if (not std::holds_alternative<from_headset::handshake>(stream_handshake))
	{
		throw std::runtime_error("No handshake received from client");
	}

	if (client_port < 0)
		stream = decltype(stream)();

	control.send(to_headset::handshake{.stream_port = port, .multilink_key = multilink_key, .endpoints = stream.endpoints(), .server_id = server_cookie()});

	info_packet = std::get<from_headset::headset_info_packet>(receive(10s, bool(stream)).first);

	last_received = std::chrono::steady_clock::now();
	active = true;

	if (state == encryption_state::pairing and not is_public_key_known)
		wivrn::add_known_key({
		        .public_key = clean_key(headset_key.public_key()),
		        .name = crypto_handshake.name,
		});
	else if (state != encryption_state::disabled)
		wivrn::update_last_connection_timestamp(clean_key(headset_key.public_key()));
}

void wivrn::wivrn_connection::reset(std::stop_token stop, TCP && tcp, std::function<void()> tick)
{
	if (stream)
		stream = decltype(stream)();

	control = std::move(tcp);
	init(stop, tick);
}

void wivrn::wivrn_connection::shutdown()
{
	active = false;
	if (stream)
		stream.shutdown();
	if (control)
		::shutdown(control.get_fd(), SHUT_RDWR);
}

std::optional<wivrn::from_headset::packets> wivrn::wivrn_connection::poll_control(int timeout)
{
	pollfd fds{};
	fds.events = POLLIN;
	fds.fd = control.get_fd();

	int r = ::poll(&fds, 1, timeout);
	if (r < 0)
		throw std::system_error(errno, std::system_category());

	if (r > 0 && (fds.revents & POLLIN))
	{
		return control.receive();
	}

	return {};
}
