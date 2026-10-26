// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "configuration.h"
#include "shard_stream.h"
#include "frame_host.h"
#include "util/u_logging.h"
#include "version.h"
#include "wivrn_packets.h"
#include "wivrn_uinput.h"
#include <array>
#include <atomic>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <system_error>
#include <unistd.h>
#include <vector>

namespace wivrn
{
// The frame shell's PC side of one headset connection: the host services' monitors and messages, and the
// headset's input, keyframe and resend requests for them. Used by the VR session and by a desktop-only session;
// Sender provides send_control, send_stream and has_stream.
template <typename Sender>
class desktop_link
{
	Sender & sender;
	std::optional<wivrn_uinput> & uinput;

	// Streamed monitors: where each one lies in the PC's whole desktop (x, y, w, h), for pointer input
	std::mutex mutex;
	std::map<uint32_t, std::array<float, 4>> regions;
	// Each monitor's video: its next frame number and its shards as sent, to send again what the headset misses
	struct video_stream
	{
		uint64_t frame = 0;
		shards::sender shards;
	};
	std::map<uint32_t, video_stream> videos;

	// Last: its callbacks use the members above until it is destroyed
	std::unique_ptr<frame_host_session> host;

	void send_all_stream(std::vector<to_headset::video_shard> & packets)
	{
		for (auto & packet: packets)
		{
			try
			{
				sender.send_stream(std::move(packet));
			}
			catch (const std::system_error &)
			{
			}
		}
	}

	// An access unit, in shards for the stream socket (UDP when available): the headset drops incomplete frames
	// and asks for a keyframe, instead of video holding up control messages
	void send_video(uint32_t surface, std::span<const uint8_t> data)
	{
		const uint8_t percent = sender.has_stream() ? fec_percent.load() : 0;
		std::vector<to_headset::video_shard> shards;
		{
			std::scoped_lock lock(mutex);
			auto & video = videos[surface];
			shards = video.shards.frame(to_headset::video_shard::desktop_streams + surface, 0, video.frame++, data, percent);
		}
		send_all_stream(shards);
	}

public:
	// Parity per frame in percent of its shards, from the headset (from_headset::link_settings)
	std::atomic<uint8_t> fec_percent = 0;

	desktop_link(Sender & sender_, std::optional<wivrn_uinput> & uinput_) :
	        sender(sender_), uinput(uinput_)
	{
		char hostname[256] = "PC";
		gethostname(hostname, sizeof(hostname) - 1);
		const auto pc_id = server_cookie();
		const auto version = std::string(display_version());
		const frame_host_identity identity{
		        .id = pc_id.c_str(),
		        .name = hostname,
		        .build = std::getenv("FRAME_SHELL_BUILD_KIND") ? std::getenv("FRAME_SHELL_BUILD_KIND") : "packaged",
		        .version = version.c_str(),
		};
		host = std::make_unique<frame_host_session>(frame_host_session::callbacks{
		                                                    .surface = [this, source = std::string(hostname)](const frame_host_surface & surface) {
			        {
				        std::scoped_lock lock(mutex);
				        regions[surface.id] = {surface.region[0], surface.region[1], surface.region[2], surface.region[3]};
			        }
			        sender.send_control(to_headset::desktop_surface{
			                .surface = surface.id,
			                .width = uint16_t(surface.width),
			                .height = uint16_t(surface.height),
			                .name = surface.name ? surface.name : "",
			                .source = source,
			                .removed = false,
			                .codec = surface.codec == 1 ? video_codec::h265 : video_codec::h264,
			        }); },
		                                                    .surface_removed = [this](uint32_t id) {
			        {
				        std::scoped_lock lock(mutex);
				        regions.erase(id);
				        videos.erase(id);
			        }
			        sender.send_control(to_headset::desktop_surface{.surface = id, .removed = true}); },
		                                                    .video = [this](uint32_t surface, std::span<const uint8_t> data) { send_video(surface, data); },
		                                                    .message = [this](std::span<const uint8_t> data) { sender.send_control(to_headset::shell_message{.payload = {data.begin(), data.end()}}); },
		                                            },
		                                            identity);
	}

	void operator()(from_headset::shell_input && event)
	{
		if (event.kind == 2)
		{
			// Pointer position on one streamed monitor: map it into the whole desktop
			std::scoped_lock lock(mutex);
			auto region = regions.find(event.surface);
			if (region == regions.end())
				return;
			event.x = region->second[0] + event.x * region->second[2];
			event.y = region->second[1] + event.y * region->second[3];
		}
		if (uinput)
			uinput->handle_shell_input(event);
	}

	void operator()(from_headset::shell_message && event)
	{
		host->receive(event.payload);
	}

	void operator()(from_headset::desktop_keyframe && event)
	{
		host->request_keyframe(event.surface);
	}

	// Sends again what the headset misses of a monitor's frame
	void resend(const from_headset::video_nack & nack)
	{
		std::vector<to_headset::video_shard> shards;
		{
			std::scoped_lock lock(mutex);
			auto it = videos.find(nack.stream - to_headset::video_shard::desktop_streams);
			if (it == videos.end())
				return;
			shards = it->second.shards.resend(nack);
		}
		send_all_stream(shards);
	}
};
} // namespace wivrn
