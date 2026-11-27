#ifndef __ANDROID__
#include "linux/frame_shell.h"
#endif
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

#include "network_interface.h"
#include "stream.h"
#include "utils/json_string.h"
#include "wivrn_serialization.h"
#if WIVRN_USE_V4L2
#include "decoder/v4l2/v4l2_decoder.h"
#endif

#include "application.h"
#include "utils/i18n.h"
#include "utils/named_thread.h"
#include "utils/xdg_base_directory.h"

#include <algorithm>
#include <arpa/inet.h>
#include <functional>
#include <ifaddrs.h>
#include <netinet/in.h>
#include <random>
#include <spdlog/spdlog.h>
#include <system_error>
#include <uni_algo/case.h>
#include <utility>

#ifndef __ANDROID__
// The interface of the local address a connected socket uses, empty if unknown
static std::string local_interface(int fd)
{
	sockaddr_storage local{};
	socklen_t size = sizeof(local);
	if (getsockname(fd, (sockaddr *)&local, &size) != 0)
		return {};
	// A v4-mapped IPv6 address carries the IPv4 one in its last 4 bytes
	std::optional<in_addr> v4;
	if (local.ss_family == AF_INET)
		v4 = ((const sockaddr_in &)local).sin_addr;
	else if (auto & a6 = (const sockaddr_in6 &)local; local.ss_family == AF_INET6 and IN6_IS_ADDR_V4MAPPED(&a6.sin6_addr))
	{
		in_addr r;
		memcpy(&r, &a6.sin6_addr.s6_addr[12], 4);
		v4 = r;
	}
	ifaddrs * list = nullptr;
	if (getifaddrs(&list) != 0)
		return {};
	std::string name;
	for (auto i = list; i and name.empty(); i = i->ifa_next)
	{
		if (not i->ifa_addr)
			continue;
		if (i->ifa_addr->sa_family == AF_INET and v4 and v4->s_addr == ((sockaddr_in *)i->ifa_addr)->sin_addr.s_addr)
			name = i->ifa_name;
		else if (i->ifa_addr->sa_family == AF_INET6 and local.ss_family == AF_INET6 and
		         memcmp(&((sockaddr_in6 *)i->ifa_addr)->sin6_addr, &((sockaddr_in6 &)local).sin6_addr, 16) == 0)
			name = i->ifa_name;
	}
	freeifaddrs(list);
	return name;
}

// Signal and rates of the headset's Wi-Fi link, from iw
static std::string link_status(const std::string & interface)
{
	if (interface.empty() or not std::ranges::all_of(interface, [](char c) { return std::isalnum((unsigned char)c) or c == '_' or c == '-'; }))
		return "null";
	std::string command = "timeout 2 iw dev " + interface + " link 2>/dev/null";
	std::optional<int> signal;
	std::optional<double> tx, rx;
	if (FILE * pipe = popen(command.c_str(), "r"))
	{
		char line[256];
		while (fgets(line, sizeof(line), pipe))
		{
			std::string_view text = line;
			auto value = [&](std::string_view key) -> std::optional<double> {
				auto at = text.find(key);
				if (at == std::string_view::npos)
					return std::nullopt;
				return std::strtod(line + at + key.size(), nullptr);
			};
			if (auto v = value("\tsignal:"); v and *v != 0)
				signal = int(*v);
			else if (auto v = value("tx bitrate:"))
				tx = *v;
			else if (auto v = value("rx bitrate:"))
				rx = *v;
		}
		pclose(pipe);
	}
	auto number = [](auto v) { return v ? fmt::format("{}", *v) : std::string("null"); };
	return fmt::format(R"({{"interface":"{}","signal":{},"tx_mbit":{},"rx_mbit":{}}})", interface, number(signal), number(tx), number(rx));
}
#endif

// FEC parity; the PC applies it right away
void scenes::stream::send_link_settings()
{
#ifndef __ANDROID__
	network_session->send_control(from_headset::link_settings{
	        .fec_percent = uint8_t(std::min<uint32_t>(pc_settings().fec_percent, 50)),
	        .frame_margin_ms = uint8_t(std::min<uint32_t>(pc_settings().frame_margin_ms, 20)),
	});
#endif
}

void scenes::stream::process_packets()
{
#ifdef __ANDROID__
	application::instance().setup_jni();
#else
	send_link_settings();
	network_status_thread = std::jthread([this, interface = local_interface(network_session->control_fd())](std::stop_token stop) {
		while (not stop.stop_requested())
		{
			auto status = link_status(interface);
			{
				std::scoped_lock lock(network_status_mutex);
				network_status = std::move(status);
			}
			for (int i = 0; i < 20 and not stop.stop_requested(); ++i)
				std::this_thread::sleep_for(std::chrono::milliseconds(100));
		}
	});
#endif
	while (state_ != state::shutdown)
	{
		try
		{
			#ifndef __ANDROID__
            if (frame_shell::enabled()) {
                apply_video_active();
                for (auto & event: frame_shell::take_input(shell_source)) {
                    if (hid_forwarding) network_session->send_control(from_headset::shell_input{event.kind, event.code, event.x, event.y, event.surface});
                }
                // The PC ends a session after a second without packets on the stream socket; tracking sends them
                // while active
                if (auto now = std::chrono::steady_clock::now(); not video_active and now - standby_keepalive > std::chrono::milliseconds(250)) {
                    standby_keepalive = now;
                    network_session->send_stream(from_headset::link_settings{
                            .fec_percent = uint8_t(std::min<uint32_t>(pc_settings().fec_percent, 50)),
                            .frame_margin_ms = uint8_t(std::min<uint32_t>(pc_settings().frame_margin_ms, 20)),
                    });
                }
                // Here rather than in render: while no PC app runs the stream scene is stalled behind the lobby and
                // doesn't render, but the shell still needs the PC's application list
                poll_shell();
            }
#endif
            network_session->poll(*this, std::chrono::milliseconds(20));
            {
                std::shared_lock lock(decoder_mutex);
                for (auto & d: decoders)
                    if (d.decoder)
                        d.decoder->request_stalled();
            }
		}
		catch (std::exception & e)
		{
			spdlog::info("Exception in network thread, exiting: {}", e.what());
			exit();
		}
	}
}

void scenes::stream::operator()(to_headset::server_message && message)
{
	switch (message.kind)
	{
		case to_headset::server_message::kind::toast:
		case to_headset::server_message::kind::toast_urgent: {
			auto toast = gui_toast.lock();
			toast->emplace(message.msg, message.kind == to_headset::server_message::kind::toast_urgent);

			gui_status_last_change = instance.now();
			break;
		}

		case to_headset::server_message::kind::error: {
			auto queue = stream_error_queue.lock();
			queue->emplace(std::move(message.msg));

			break;
		}
	}
}

void scenes::stream::operator()(to_headset::video_shard && shard)
{
	if (shard.stream >= to_headset::video_shard::desktop_streams)
	{
#ifndef __ANDROID__
		if (desktop)
			(*desktop)(std::move(shard));
#endif
		return;
	}
	std::shared_lock lock(decoder_mutex);
	if (not video_stream_description or shard.generation != video_stream_description->generation)
		return;
#ifndef __ANDROID__
	if (frame_shell::enabled() and shard.index == 0 and shell_encoded_width and shell_encoded_height and
	    shard.payload.size() >= serialized_size(to_headset::video_frame_info{}))
	{
		deserialization_packet packet({}, shard.payload);
		const auto info = packet.deserialize<to_headset::video_frame_info>();
		const auto & parameters = info.view_info.foveation[0];
		if (parameters.x.source_size and parameters.y.source_size)
		{
			auto extent = stream_defoveator::defoveated_size(parameters);
			bool active = extent.width > shell_encoded_width or extent.height > shell_encoded_height;
			auto debug = fmt::format(R"({{"encoded_width":{},"encoded_height":{},"render_width":{},"render_height":{},"foveated":{}}})",
			                         shell_encoded_width, shell_encoded_height, extent.width, extent.height, active);
			if (debug != shell_video_debug)
			{
				spdlog::info("Received VR foveation: {}x{} encoded, {}x{} rendered per eye; {}", shell_encoded_width, shell_encoded_height, extent.width, extent.height, active ? "active" : "off");
				shell_video_debug = std::move(debug);
			}
		}
	}
#endif
	auto idx = shard.stream;
	if (idx < decoders.size() and decoders[idx].decoder)
		decoders[idx].decoder->push_shard(std::move(shard));
}

void scenes::stream::operator()(to_headset::feature_control && control)
{
	switch (control.f)
	{
		case wivrn::to_headset::feature_control::hid_input:
			hid_forwarding = control.state;
			if (not control.state and (application::get_config().forward_keyboard or application::get_config().forward_mouse or application::get_config().forward_gamepad))
			{
				auto toast = gui_toast.lock();
				toast->emplace(_("The server does not allow forwarded input devices"), true);
				gui_status_last_change = instance.now();
			}
			return;
		case wivrn::to_headset::feature_control::microphone:
			microphone_requested = control.state;
			if (audio_handle)
				audio_handle->set_mic_state(background_active && microphone_requested);
			return;
	}
}

void scenes::stream::operator()(to_headset::audio_stream_description && desc)
{
	audio_handle.emplace(desc, *network_session, instance);
	audio_handle->set_mic_state(background_active && microphone_requested);
}

void scenes::stream::operator()(to_headset::video_stream_description && desc)
{
	if (not video_active)
	{
		standby_video = std::move(desc);
		return;
	}
#ifndef __ANDROID__
    shell_encoded_width = desc.width;
    shell_encoded_height = desc.height;
    shell_video_debug = "null";
#endif
	setup(desc);

	if (not tracking_thread)
	{
		tracking_thread = utils::named_thread("tracking_thread", &stream::tracking, this);
	}
#if WIVRN_FRAME_EYES
	if (not eye_tracker and application::get_config().eye_expressions)
		eye_tracker = std::make_unique<frame_eyes::tracker>(
		        xdg_data_home() / "frame-shell",
		        application::get_config().eye_expressions_divisor,
		        [this](from_headset::frame_eyes && eyes) {
			        if (state_ == state::shutdown or not video_active)
				        return;
			        try
			        {
				        network_session->send_stream(std::move(eyes));
			        }
			        catch (const wivrn::socket_shutdown &)
			        {
				        exit();
			        }
			        catch (const std::system_error & e)
			        {
				        spdlog::info("Exception in eye tracking sender, exiting: {}", e.what());
				        exit();
			        }
		        },
		        [this]() { return instance.now(); });
#endif
}

void scenes::stream::operator()(to_headset::refresh_rate_change && rate)
{
	spdlog::info("refresh rate change request: {}", rate.hz);
	session.set_refresh_rate(rate.hz);
	std::shared_lock lock(decoder_mutex);
	if (video_stream_description)
		video_stream_description->refresh_rate = rate.hz;
}

void scenes::stream::operator()(to_headset::stream_tab_change && tab)
{
	next_gui_status = tab.tab;
}

void scenes::stream::operator()(to_headset::timesync_query && query)
{
	network_session->send_stream_reply(from_headset::timesync_response{
	        .query = query.query,
	        .response = instance.now(),
	});
}

void scenes::stream::operator()(audio_data && data)
{
	if (audio_handle)
		(*audio_handle)(std::move(data));
}

void scenes::stream::send_feedback(const wivrn::from_headset::feedback & feedback)
{
	try
	{
		network_session->send_stream(wivrn::from_headset::feedback{feedback});
	}
	catch (std::exception & e)
	{
		spdlog::warn("Exception while sending feedback packet: {}", e.what());
	}
}

// Asks for lost video shards again, over the stream socket so that they come back within the frame
void scenes::stream::send_nack(wivrn::from_headset::video_nack && nack)
{
	try
	{
		network_session->send_stream(std::move(nack));
	}
	catch (std::exception & e)
	{
		spdlog::warn("Exception while sending a retransmission request: {}", e.what());
	}
}

void scenes::stream::operator()(to_headset::application_list && l)
{
	apps(std::move(l));
}

void scenes::stream::operator()(to_headset::application_icon && icon)
{
	apps(std::move(icon));
}

void scenes::stream::operator()(to_headset::running_applications && apps)
{
	*running_applications.lock() = std::move(apps);
}

// Frame shell: the lobby made this PC present VR or put it on standby. Activating asks the PC for its video, whose
// description then sets up the decoders and tracking as on connection.
void scenes::stream::apply_video_active()
{
	const bool background = wanted_background;
	if (background != background_active)
	{
		background_active = background;
		if (audio_handle)
			audio_handle->set_mic_state(background_active && microphone_requested);
	}
	bool active = wanted_active;
	if (active == video_active)
		return;
	spdlog::info("{}: VR {}", apps.server(), active ? "active" : "on standby");
	video_active = active;
	if (not active)
		teardown_video();
	network_session->send_control(from_headset::video_active{.active = active});
	if (audio_handle)
		audio_handle->set_mic_state(background_active && microphone_requested);
	if (active and standby_video)
	{
		auto desc = std::move(*standby_video);
		standby_video.reset();
		(*this)(std::move(desc));
	}
#ifndef __ANDROID__
	if (frame_shell::enabled())
		frame_shell::connected(shell_source, true, active);
#endif
}

void scenes::stream::start_application(std::string appid)
{
	network_session->send_control(wivrn::from_headset::start_app{
	        .app_id = std::move(appid),
	});
}

void scenes::stream::operator()(to_headset::desktop_surface && surface)
{
#ifndef __ANDROID__
	if (desktop)
		(*desktop)(std::move(surface));
#endif
}


#ifndef __ANDROID__
static bool is_stream_command(std::string_view command)
{
	return command == "disconnect" or command.starts_with("present ") or command.starts_with("start_app ") or command.starts_with("set_active_app ") or command.starts_with("stop_app ");
}

bool scenes::stream::resolution_locked()
{
	return not frame_shell::activity(shell_source).known or not frame_shell::activity(shell_source).defaults.empty();
}

void scenes::stream::setting_changed(std::string_view key)
{
	const auto & config = application::get_config();
	if (key == "fec" or key == "frame_margin")
		send_link_settings();
	// Size and codec changes rebuild the server's encoders
	else if (key == "resolution" or key == "foveation" or key == "codec")
		network_session->send_control(stream_settings());
	else if (key == "bitrate")
		network_session->send_control(from_headset::settings_changed{
		        .preferred_refresh_rate = config.preferred_refresh_rate,
		        .minimum_refresh_rate = config.minimum_refresh_rate.value_or(0),
		        .fps_divider = config.fps_divider,
		        .bitrate_bps = pc_settings().bitrate_bps,
		        .mirror_gamepad = config.forward_gamepad,
		        .enabled_body_parts = config.body_part_mask,
		});
}

void scenes::stream::set_link_mode(std::string_view mode)
{
	std::scoped_lock lock(shell_link_mutex);
	shell_link_request = std::string(mode);
}

void scenes::stream::poll_shell()
{
	// Commands for this PC: "on <source> <command>", and unprefixed ones while it presents VR
	const std::string prefix = fmt::format("on {} ", shell_source);
	const bool active = video_active;
	while (auto command = frame_shell::take_command([&](std::string_view command) {
		       return command.starts_with(prefix) or (active and is_stream_command(command));
	       }))
	{
		if (command->starts_with(prefix))
			command->erase(0, prefix.size());
		spdlog::info("Shell command: {}", *command);
		if (*command == "disconnect")
		{
			frame_shell::user_disconnected = true;
			exit();
		}
		else if (command->starts_with("present "))
			frame_shell::request_present(command->substr(8));
		else if (command->starts_with("start_app "))
			start_application(command->substr(10));
		else if (command->starts_with("stop_app "))
		{
			try { network_session->send_control(from_headset::stop_application{.id = uint32_t(std::stoul(command->substr(9)))}); } catch (std::exception &) {}
		}
		else if (command->starts_with("set_active_app "))
		{
			try
			{
				network_session->send_control(from_headset::set_active_application{.id = uint32_t(std::stoul(command->substr(15)))});
			}
			catch (std::exception &)
			{}
		}
	}

	if (auto now = std::chrono::steady_clock::now(); now >= shell_next_apps_query)
	{
		shell_next_apps_query = now + std::chrono::seconds(30);
		network_session->send_control(from_headset::get_application_list{
		        .language = application::get_messages_info().language,
		        .country = application::get_messages_info().country,
		        .variant = application::get_messages_info().variant,
		});
	}

	// The PC's running applications, for the shell's app list
	if (auto now = std::chrono::steady_clock::now(); now >= shell_next_running_query)
	{
		shell_next_running_query = now + std::chrono::seconds(2);
		network_session->send_control(from_headset::get_running_applications{});
	}

	for (auto & message: frame_shell::take_host_messages(shell_source))
		network_session->send_control(from_headset::shell_message{.payload = std::move(message)});

	// Publish each connection's stream state at most every 100 ms.
	auto now = std::chrono::steady_clock::now();
	if (now < shell_next_state)
		return;
	shell_next_state = now + std::chrono::milliseconds(100);
#if WIVRN_FRAME_EYES
	if (now >= shell_next_debug)
	{
		shell_next_debug = now + std::chrono::seconds(1);
		shell_eyes_debug = eye_tracker ? eye_tracker->statistics_json() : "null";
	}
#endif
	if (now >= shell_next_links)
	{
		shell_next_links = now + std::chrono::seconds(1);
		if (shell_link_mode.empty())
			shell_link_mode = application::get_config().link_mode;
		std::optional<std::string> request;
		{
			std::scoped_lock lock(shell_link_mutex);
			request = std::exchange(shell_link_request, std::nullopt);
		}
		if (request)
		{
			// A new mode starts from every link; "preferred" pins once the preferred link is seen
			shell_link_mode = *request;
			shell_link_pinned.clear();
			shell_link_lost = {};
			shell_link_retry = {};
			network_session->set_multilink_enabled(shell_link_mode != "fastest");
		}
		const auto paths = network_session->links();
		const auto & preferred_interface = application::get_config().preferred_interface;
		// Preferred first: stream over the preferred link while it works, and over every link while it does not
		if (shell_link_mode == "preferred" and not preferred_interface.empty())
		{
			if (shell_link_pinned.empty())
			{
				const auto preferred = std::ranges::find_if(paths, [&](const auto & link) {
					return link.local.interface == preferred_interface and link.active and link.rtt_ms > 0;
				});
				if (now >= shell_link_retry and preferred != paths.end() and network_session->set_single_interface(preferred->local.interface))
				{
					shell_link_pinned = preferred->local.interface;
					spdlog::info("{}: streaming over {} only", apps.server(), shell_link_pinned);
				}
			}
			else if (std::ranges::any_of(paths, [&](const auto & link) { return link.local.interface == shell_link_pinned and link.active; }))
				shell_link_lost = {};
			else if (shell_link_lost == std::chrono::steady_clock::time_point{})
				shell_link_lost = now;
			else if (now - shell_link_lost > std::chrono::seconds(3))
			{
				spdlog::warn("{}: {} stopped working, streaming over every link", apps.server(), shell_link_pinned);
				shell_link_pinned.clear();
				shell_link_lost = {};
				shell_link_retry = now + std::chrono::seconds(10);
				network_session->set_multilink_enabled(true);
			}
		}

		// One entry per interface: the paths are its address pairs (IPv4 and IPv6, each PC address), and
		// the ones never validated have no measurements
		struct interface_link
		{
			std::string name;
			double rtt_ms = 0;
			double jitter_ms = 0;
			double loss = 0;
			double capacity_bps = 0;
			double received_bps = 0;
			double sent_bps = 0;
			bool active = false;
		};
		std::vector<interface_link> interfaces;
		for (const auto & link: paths)
		{
			if (not link.active and not link.ever_active and link.rtt_ms <= 0)
				continue;
			auto it = std::ranges::find(interfaces, link.local.interface, &interface_link::name);
			if (it == interfaces.end())
				it = interfaces.insert(interfaces.end(), interface_link{.name = link.local.interface});
			if (link.rtt_ms > 0 and (it->rtt_ms <= 0 or link.rtt_ms < it->rtt_ms))
			{
				it->rtt_ms = link.rtt_ms;
				it->jitter_ms = link.jitter_ms;
				it->loss = link.loss;
			}
			it->capacity_bps = std::max(it->capacity_bps, double(link.capacity_bps));
			it->received_bps += double(link.received_bps);
			it->sent_bps += double(link.sent_bps);
			it->active |= link.active;
		}
		std::ranges::stable_sort(interfaces, std::greater<>{}, &interface_link::active);
		std::string links = "[";
		for (const auto & link: interfaces)
		{
			links += fmt::format(R"({}{{"name":{},"rtt_ms":{:.1f},"jitter_ms":{:.1f},"loss_percent":{:.1f},"capacity_mbit":{:.0f},"rx_mbit":{:.1f},"tx_mbit":{:.1f},"active":{}}})",
			                     links.size() > 1 ? "," : "",
			                     json_string(wivrn::interface_display_name(link.name)),
			                     link.rtt_ms,
			                     link.jitter_ms,
			                     link.loss * 100,
			                     link.capacity_bps * 1e-6,
			                     link.received_bps * 1e-6,
			                     link.sent_bps * 1e-6,
			                     link.active);
		}
		shell_links = links + "]";
	}
	std::string json = fmt::format(R"({{"server":{},"apps":[)", json_string(apps.server()));
	bool first = true;
	for (const auto & [id, name, icon]: apps.list())
	{
		json += fmt::format(R"({}{{"id":{},"name":{},"icon":{}}})", first ? "" : ",", json_string(id), json_string(name), json_string(icon));
		first = false;
	}
	json += R"(],"running":[)";
	first = true;
	for (const auto & app: running_applications.lock()->applications)
	{
		json += fmt::format(R"({}{{"id":{},"name":{},"active":{},"overlay":{}}})", first ? "" : ",", app.id, json_string(app.name), app.active, app.overlay);
		first = false;
	}
	json += R"(],"settings":)" + frame_shell::settings_json(application::get_config(), pc_settings()) + R"(,"video_debug":)" + shell_video_debug + R"(,"eyes_debug":)" + shell_eyes_debug;
	{
		std::scoped_lock lock(shell_vr_debug_mutex);
		json += R"(,"vr_debug":)" + shell_vr_debug;
	}
	json += fmt::format(R"(,"multilink":{{"mode":{},"pinned":{},"links":{}}})",
	                    json_string(shell_link_mode),
	                    shell_link_pinned.empty() ? "null" : json_string(wivrn::interface_display_name(shell_link_pinned)),
	                    shell_links);
	{
		std::scoped_lock lock(network_status_mutex);
		json += R"(,"network":)" + network_status + "}";
	}

	if (json != shell_state)
	{
		shell_state = json;
		frame_shell::stream_state(shell_source, shell_state.c_str());
	}
}
#endif

void scenes::stream::operator()(to_headset::shell_message && message)
{
#ifndef __ANDROID__
	if (frame_shell::enabled())
		frame_shell::host_message(shell_source, message.payload);
#endif
}

void scenes::stream::operator()(to_headset::network_report && report)
{
	*network_report.lock() = std::move(report);
}
