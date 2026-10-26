// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "decoder/decoder.h"
#include "decoder/frame_sink.h"
#include "wivrn_packets.h"
#include <chrono>
#include <cstdint>
#include <deque>
#include <functional>
#include <map>
#include <memory>
#include <optional>
#include <shared_mutex>
#include <string>
#include <vector>
#include <vulkan/vulkan_raii.hpp>

namespace frame_shell
{
// A new number for a connected PC: the shell keys its monitors and messages by it
uint32_t next_source();

// One PC's monitors (desktop_surface, desktop_video), each decoded by its own V4L2 decoder sized for it, whose
// frames go to the shell under the PC's source number. Packets arrive on one network thread.
class desktop_receiver : public wivrn::frame_sink, public std::enable_shared_from_this<desktop_receiver>
{
public:
	struct link
	{
		std::function<void(wivrn::from_headset::desktop_keyframe &&)> keyframe;
		std::function<void(wivrn::from_headset::desktop_nack &&)> nack;
	};

	const uint32_t source;

	desktop_receiver(uint32_t source, vk::raii::Device &, vk::raii::PhysicalDevice &, uint32_t queue_family_index, link);
	desktop_receiver(const desktop_receiver &) = delete;
	desktop_receiver & operator=(const desktop_receiver &) = delete;
	~desktop_receiver() override;

	void operator()(wivrn::to_headset::desktop_surface &&);
	void operator()(wivrn::to_headset::desktop_video &&);
	void push_blit_handle(wivrn::shard_accumulator * decoder, std::shared_ptr<wivrn::shard_accumulator::blit_handle> handle) override;

private:
	vk::raii::Device & device;
	vk::raii::PhysicalDevice & physical_device;
	uint32_t queue_family_index;
	link sender;

	// A stream's address is the accumulator tag push_blit_handle recognises
	struct desktop_stream
	{
		uint32_t surface;
		uint16_t width, height;
		wivrn::video_codec codec = wivrn::video_codec::h264;
		std::shared_ptr<wivrn::decoder> decoder;
		uint64_t frame_index = 0;
		std::string name;
		// Until a frame with SPS (a keyframe) arrives, after a loss or a decoder failure
		bool wait_keyframe = true;
		std::chrono::steady_clock::time_point keyframe_requested{};
		// Frames after the last one decoded, still arriving or waiting for an earlier one (desktop_video)
		struct pending_frame
		{
			std::vector<std::vector<uint8_t>> shards;
			std::vector<std::vector<uint8_t>> parity;
			size_t received = 0;
			uint32_t size = 0;
			bool complete = false;
			// Asked to be sent again (from_headset::desktop_nack): how often, when first
			int requests = 0;
			std::chrono::steady_clock::time_point requested{};
		};
		std::map<uint32_t, pending_frame> pending;
		std::optional<uint32_t> last_frame;
		// Recent decoder failures, to stop recreating a decoder that keeps failing
		std::deque<std::chrono::steady_clock::time_point> failures;
	};
	using streams_t = std::map<uint32_t, std::unique_ptr<desktop_stream>>;

	// Written only by the network thread
	std::shared_mutex mutex;
	streams_t streams;

	std::unique_ptr<desktop_stream> make_stream(uint32_t surface, uint16_t width, uint16_t height, const std::string & name, wivrn::video_codec codec) noexcept;
	void frame_lost(desktop_stream &);
	void deliver_frames(streams_t::iterator);
	bool decode_frame(streams_t::iterator, desktop_stream::pending_frame &);
};
} // namespace frame_shell
