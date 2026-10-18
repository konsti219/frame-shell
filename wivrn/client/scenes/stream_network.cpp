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

#include "stream.h"
#include "fec.h"
#include "utils/json_string.h"
#if WIVRN_USE_V4L2
#include "decoder/v4l2/v4l2_decoder.h"
#endif

#include "application.h"
#include "utils/i18n.h"
#include "utils/named_thread.h"
#include "utils/xdg_base_directory.h"

#include <arpa/inet.h>
#include <ifaddrs.h>
#include <netinet/in.h>
#include <random>
#include <spdlog/spdlog.h>
#include <system_error>
#include <uni_algo/case.h>

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
	        .fec_percent = uint8_t(std::min<uint32_t>(application::get_config().fec_percent, 50)),
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
                frame_shell::input event;
                for (int count = 0; count < 256 && frame_shell::input_poll(&event); ++count) {
                    if (hid_forwarding) network_session->send_control(from_headset::shell_input{event.kind, event.code, event.x, event.y, event.surface});
                }
                // Here rather than in render: while no PC app runs the stream scene is stalled behind the lobby and
                // doesn't render, but the shell still needs the PC's application list
                poll_shell();
            }
#endif
            network_session->poll(*this, std::chrono::milliseconds(20));
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

// Testing (simulated_loss_percent): whether to drop this video shard, losing runs like Wi-Fi does
static bool simulated_loss()
{
	const auto & config = application::get_config();
	if (config.simulated_loss_percent <= 0)
		return false;
	thread_local std::minstd_rand random{std::random_device{}()};
	thread_local bool losing = false;
	float burst = std::max(1u, config.simulated_loss_burst);
	std::uniform_real_distribution<float> uniform;
	// Two-state model: runs average `burst` shards, and the share lost matches the setting
	float enter = config.simulated_loss_percent / 100 / burst / (1 - config.simulated_loss_percent / 100);
	losing = losing ? uniform(random) >= 1 / burst : uniform(random) < enter;
	return losing;
}

void scenes::stream::operator()(to_headset::video_stream_data_shard && shard)
{
	if (simulated_loss())
		return;
	std::shared_lock lock(decoder_mutex);
	if (not video_stream_description or shard.generation != video_stream_description->generation)
		return;
#ifndef __ANDROID__
    if (frame_shell::enabled() && shard.view_info && shell_encoded_width && shell_encoded_height) {
        const auto & parameters = shard.view_info->foveation[0];
        if (parameters.x.source_size && parameters.y.source_size) {
            auto extent = stream_defoveator::defoveated_size(parameters);
            bool active = extent.width > shell_encoded_width || extent.height > shell_encoded_height;
            auto debug = fmt::format(R"({{"encoded_width":{},"encoded_height":{},"render_width":{},"render_height":{},"foveated":{}}})",
                shell_encoded_width, shell_encoded_height, extent.width, extent.height, active);
            if (debug != shell_video_debug) {
                spdlog::info("Received VR foveation: {}x{} encoded, {}x{} rendered per eye; {}", shell_encoded_width, shell_encoded_height, extent.width, extent.height, active ? "active" : "off");
                shell_video_debug = std::move(debug);
            }
        }
    }
#endif
	uint8_t idx = shard.stream_item_idx;
	if (idx >= decoders.size() or not decoders[idx].decoder)
	{
		// We don't know (yet?) about this stream, ignore packet
		return;
	}
	decoders[idx].decoder->push_shard(std::move(shard));
}

void scenes::stream::operator()(to_headset::video_stream_parity_shard && parity)
{
	if (simulated_loss())
		return;
	std::shared_lock lock(decoder_mutex);
	if (video_stream_description and parity.generation == video_stream_description->generation and
	    parity.stream_item_idx < decoders.size() and decoders[parity.stream_item_idx].decoder)
		decoders[parity.stream_item_idx].decoder->push_parity(std::move(parity));
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
			if (audio_handle)
				audio_handle->set_mic_state(control.state);
			return;
	}
}

void scenes::stream::operator()(to_headset::audio_stream_description && desc)
{
	audio_handle.emplace(desc, *network_session, instance);
}

void scenes::stream::operator()(to_headset::video_stream_description && desc)
{
#ifndef __ANDROID__
    shell_encoded_width = desc.width;
    shell_encoded_height = desc.height;
    shell_video_debug = "null";
#endif
	setup(desc);
#ifndef __ANDROID__
	if (frame_shell::enabled()) frame_shell::connected(true);
#endif

	if (not tracking_thread)
	{
		tracking_thread = utils::named_thread("tracking_thread", &stream::tracking, this);
	}
#if WIVRN_FRAME_EYES
	if (not eye_tracker and application::get_config().eye_expressions)
		eye_tracker = std::make_unique<frame_eyes::tracker>(
		        xdg_data_home() / "frame-shell" / "eye_split96_uncapped_fp16.pte",
		        application::get_config().eye_expressions_divisor,
		        [this](from_headset::frame_eyes && eyes) {
			        if (state_ == state::shutdown)
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
	network_session->send_stream(from_headset::timesync_response{
	        .query = query.query,
	        .response = instance.now(),
	});
}

void scenes::stream::operator()(audio_data && data)
{
	if (simulated_loss())
		return;
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
	if (not application::get_config().retransmit)
		return;
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

void scenes::stream::start_application(std::string appid)
{
	network_session->send_control(wivrn::from_headset::start_app{
	        .app_id = std::move(appid),
	});
}

void scenes::stream::operator()(to_headset::desktop_surface && surface)
{
#if WIVRN_USE_V4L2
	if (!frame_shell::enabled())
		return;
	// One WiVRn connection for now; more PCs get their own source numbers
	frame_shell::desktop_surface_info info{
	        .source = 0,
	        .surface = surface.surface,
	        .width = surface.width,
	        .height = surface.height,
	        .removed = surface.removed,
	};
	std::snprintf(info.name, sizeof(info.name), "%s", surface.name.c_str());
	std::snprintf(info.source_name, sizeof(info.source_name), "%s", surface.source.c_str());

	std::unique_ptr<desktop_stream> old;
	auto it = desktop_streams.find(surface.surface);
	if (it != desktop_streams.end() &&
	    (surface.removed || it->second->width != surface.width || it->second->height != surface.height ||
	     it->second->codec != surface.codec))
	{
		std::unique_lock lock(desktop_mutex);
		old = std::move(it->second);
		desktop_streams.erase(it);
		it = desktop_streams.end();
	}
	// Destroy outside the lock: the decoder joins threads that may be waiting for it in push_blit_handle
	old.reset();
	if (!surface.removed && it == desktop_streams.end())
	{
		auto stream = make_desktop_stream(surface.surface, surface.width, surface.height, surface.name, surface.codec);
		spdlog::info("Desktop surface {} ({} on {}): {}x{}", surface.surface, surface.name, surface.source, surface.width, surface.height);
		std::unique_lock lock(desktop_mutex);
		desktop_streams.emplace(surface.surface, std::move(stream));
	}
	frame_shell::desktop_surface(&info);
#endif
}

std::unique_ptr<scenes::stream::desktop_stream> scenes::stream::make_desktop_stream(uint32_t surface, uint16_t width, uint16_t height, const std::string & name, video_codec codec) noexcept
{
	auto stream = std::make_unique<desktop_stream>();
	stream->surface = surface;
	stream->width = width;
	stream->height = height;
	stream->name = name;
	stream->codec = codec;
#if WIVRN_USE_V4L2
	to_headset::video_stream_description description{
	        .width = width,
	        .height = height,
	        .codec = {codec, codec, codec},
	};
	stream->decoder = std::make_shared<wivrn::v4l2::decoder>(
	        device, physical_device, queue_family_index, description, 0, shared_from_this(),
	        reinterpret_cast<shard_accumulator *>(stream.get()));
#endif
	return stream;
}

// A desktop video frame went missing: decoding resumes at the next keyframe, which the PC is asked for
void scenes::stream::desktop_frame_lost(desktop_stream & stream)
{
	stream.wait_keyframe = true;
	auto now = std::chrono::steady_clock::now();
	if (now - stream.keyframe_requested < std::chrono::milliseconds(250))
		return;
	stream.keyframe_requested = now;
	try
	{
		network_session->send_control(from_headset::desktop_keyframe{.surface = stream.surface});
	}
	catch (...)
	{
	}
}

void scenes::stream::operator()(to_headset::desktop_video && video)
{
#if WIVRN_USE_V4L2
	if (simulated_loss())
		return;
	auto it = desktop_streams.find(video.surface);
	if (it == desktop_streams.end() || !it->second->decoder)
		return;
	auto & stream = *it->second;
	if (video.shard_count == 0 or video.parity_count > video.shard_count or
	    uint32_t(video.shard_count) + video.parity_count > 65535 or
	    video.shard >= uint32_t(video.shard_count) + video.parity_count or
	    video.frame_size == 0 or video.payload.empty() or
	    (video.parity_count and (video.payload.size() != fec::block_size or
	        video.frame_size > size_t(video.shard_count) * fec::block_size or
	        video.frame_size <= size_t(video.shard_count - 1) * fec::block_size)))
		return;
	if (stream.last_frame and int32_t(video.frame - *stream.last_frame) <= 0)
		return;

	auto & frame = stream.pending[video.frame];
	if (frame.shards.empty())
	{
		frame.shards.assign(video.shard_count, {});
		frame.parity.assign(video.parity_count, {});
		frame.size = video.frame_size;
	}
	if (video.shard_count != frame.shards.size() or video.parity_count != frame.parity.size() or video.frame_size != frame.size)
		return;
	if (not frame.complete)
	{
		if (video.shard < video.shard_count)
		{
			if (frame.shards[video.shard].empty())
			{
				frame.shards[video.shard] = std::move(video.payload);
				++frame.received;
			}
		}
		else
			frame.parity[video.shard - video.shard_count] = std::move(video.payload);
		if (frame.received == frame.shards.size() or fec::decode(frame.shards, frame.parity))
		{
			if (not frame.parity.empty())
				frame.shards.back().resize(frame.size - (frame.shards.size() - 1) * fec::block_size);
			frame.complete = true;
		}
	}
	deliver_desktop_frames(it);
#endif
}

#if WIVRN_USE_V4L2
// Decodes a monitor's complete frames in order. Once a later frame arrives, an incomplete one asks for what it lacks
// (from_headset::desktop_nack) and later frames wait; if that doesn't come back in time it's given up and decoding
// resumes at the next keyframe.
void scenes::stream::deliver_desktop_frames(std::map<uint32_t, std::unique_ptr<desktop_stream>>::iterator it)
{
	auto & stream = *it->second;
	const bool retransmit = application::get_config().retransmit;
	while (not stream.pending.empty())
	{
		uint32_t expected = stream.last_frame ? *stream.last_frame + 1 : stream.pending.begin()->first;
		if (auto first = stream.pending.find(expected); first != stream.pending.end() and first->second.complete)
		{
			auto frame = std::move(first->second);
			stream.pending.erase(first);
			stream.last_frame = expected;
			if (not decode_desktop_frame(it, frame))
				return;
			continue;
		}
		// Nothing later yet: it may still be arriving
		if (std::prev(stream.pending.end())->first == expected)
			return;
		// A frame missing altogether gets an empty entry, which asks for all its shards
		auto & missing = stream.pending[expected];
		auto now = std::chrono::steady_clock::now();
		if (retransmit and (missing.requests == 0 or (missing.requests == 1 and now - missing.requested > std::chrono::milliseconds(10))))
		{
			if (missing.requests++ == 0)
				missing.requested = now;
			from_headset::desktop_nack nack{.surface = stream.surface, .frame = expected};
			for (size_t i = 0; i < missing.shards.size(); ++i)
				if (missing.shards[i].empty())
					nack.shards.push_back(i);
			try
			{
				network_session->send_stream(std::move(nack));
			}
			catch (std::exception &)
			{
			}
			return;
		}
		if (retransmit and now - missing.requested < std::chrono::milliseconds(40) and stream.pending.size() < 8)
			return;
		stream.pending.erase(expected);
		stream.last_frame = expected;
		desktop_frame_lost(stream);
	}
}

// False if the decoder failed and the stream was replaced
bool scenes::stream::decode_desktop_frame(std::map<uint32_t, std::unique_ptr<desktop_stream>>::iterator it, desktop_stream::pending_frame & frame)
{
	auto & stream = *it->second;
	if (stream.wait_keyframe)
	{
		if (not wivrn::decoder::has_sps(frame.shards.front(), stream.codec))
		{
			desktop_frame_lost(stream);
			return true;
		}
		stream.wait_keyframe = false;
	}

	try
	{
		std::vector<std::span<const uint8_t>> chunks(frame.shards.begin(), frame.shards.end());
		stream.decoder->push_data(chunks, stream.frame_index, false);
		// iris sometimes stops taking input of one session when several start together
		if (auto v4l2 = std::dynamic_pointer_cast<wivrn::v4l2::decoder>(stream.decoder); v4l2 and v4l2->stalled(std::chrono::seconds(1)))
			throw std::runtime_error("iris decoder stopped taking input");
		stream.decoder->frame_completed({.frame_index = stream.frame_index, .stream_index = 255}, {});
		++stream.frame_index;
	}
	catch (const std::exception & e)
	{
		// iris sometimes puts a fresh session in its error state (seen while several decoders start together):
		// replace the decoder and resume at the next keyframe, unless it keeps failing
		auto now = std::chrono::steady_clock::now();
		auto failures = std::move(stream.failures);
		std::erase_if(failures, [&](auto when) { return now - when > std::chrono::seconds(30); });
		failures.push_back(now);
		bool retry = failures.size() <= 5;
		spdlog::error("Desktop decoder for {}: {}{}", stream.name, e.what(), retry ? ", recreating it" : ", giving up");
		if (not retry)
			std::terminate();

		std::unique_ptr<desktop_stream> failed;
		{
			std::unique_lock lock(desktop_mutex);
			failed = std::move(it->second);
		}
		// Destroy outside the lock (the decoder joins its threads); its held frames go first
		auto surface = failed->surface;
		auto width = failed->width;
		auto height = failed->height;
		auto name = failed->name;
		auto codec = failed->codec;
		failed.reset();

		auto replacement = make_desktop_stream(surface, width, height, name, codec);
		replacement->failures = std::move(failures);
		desktop_frame_lost(*replacement);
		std::unique_lock lock(desktop_mutex);
		it->second = std::move(replacement);
		return false;
	}
	return true;
}
#endif

#ifndef __ANDROID__
static bool is_stream_command(std::string_view command)
{
	return command == "disconnect" or command.starts_with("start_app ") or command.starts_with("set_active_app ") or command.starts_with("setting ") or command.starts_with("stop_app ");
}

void scenes::stream::poll_shell()
{
	while (auto command = frame_shell::take_command(is_stream_command))
	{
		spdlog::info("Shell command: {}", *command);
		if (command->starts_with("setting "))
		{
			if (command->starts_with("setting resolution ") &&
			    std::ranges::any_of(running_applications.lock()->applications, [](const auto & app) { return !app.overlay; }))
				continue;
			auto & config = application::get_config();
			if (not frame_shell::apply_setting(config, *command))
				continue;
			if (command->starts_with("setting fec "))
			{
				send_link_settings();
				continue;
			}
			// Size and codec changes rebuild the server's encoders
			if (command->starts_with("setting resolution ") or command->starts_with("setting foveation ") or command->starts_with("setting codec "))
				network_session->send_control(stream_settings());
			else
				network_session->send_control(from_headset::settings_changed{
				    .preferred_refresh_rate = config.preferred_refresh_rate,
				    .minimum_refresh_rate = config.minimum_refresh_rate.value_or(0),
				    .fps_divider = config.fps_divider,
				    .bitrate_bps = config.bitrate_bps,
				    .mirror_gamepad = config.forward_gamepad,
				    .enabled_body_parts = config.body_part_mask,
				});
		}
		else if (*command == "disconnect")
		{
			frame_shell::user_disconnected = true;
			exit();
		}
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

	// The PC's running applications, for the shell's app list
	if (auto now = std::chrono::steady_clock::now(); now >= shell_next_running_query)
	{
		shell_next_running_query = now + std::chrono::seconds(2);
		network_session->send_control(from_headset::get_running_applications{});
	}

	for (auto & message: frame_shell::take_host_messages())
		network_session->send_control(from_headset::shell_message{.payload = std::move(message)});

#if WIVRN_FRAME_EYES
	if (auto now = std::chrono::steady_clock::now(); now >= shell_next_debug)
	{
		shell_next_debug = now + std::chrono::seconds(1);
		shell_eyes_debug = eye_tracker ? eye_tracker->statistics_json() : "null";
	}
#endif
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
	json += R"(],"settings":)" + frame_shell::settings_json(application::get_config()) + R"(,"video_debug":)" + shell_video_debug + R"(,"eyes_debug":)" + shell_eyes_debug;
	{
		std::scoped_lock lock(shell_vr_debug_mutex);
		json += R"(,"vr_debug":)" + shell_vr_debug;
	}
	{
		std::scoped_lock lock(network_status_mutex);
		json += R"(,"network":)" + network_status + "}";
	}

	if (json != shell_state)
	{
		shell_state = json;
		frame_shell::stream_state(shell_state.c_str());
	}
}
#endif

void scenes::stream::operator()(to_headset::shell_message && message)
{
#ifndef __ANDROID__
	if (frame_shell::enabled())
		frame_shell::host_message(message.payload);
#endif
}
