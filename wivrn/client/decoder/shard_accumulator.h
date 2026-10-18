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

#include "decoder.h"
#include "wivrn_packets.h"
#include <openxr/openxr.h>

#include <atomic>
#include <memory>
#include <optional>
#include <vector>
#include <vulkan/vulkan_raii.hpp>

namespace xr
{
class instance;
}

namespace scenes
{
class stream;
}

namespace wivrn
{


class shard_accumulator
{
	std::shared_ptr<decoder> decoder_;

public:
	using data_shard = wivrn::to_headset::video_stream_data_shard;
	struct shard_set
	{
		size_t submitted = 0;
		std::vector<std::optional<data_shard>> data;
		// Parity blocks (fec.h), empty when missing, and the frame's data shard count they announce
		std::vector<std::vector<uint8_t>> parity;
		uint16_t data_count = 0;
		// How often each data shard was asked to be sent again (from_headset::video_nack), and when last
		std::vector<uint8_t> requested;
		XrTime last_request = 0;
		// When the last data or parity shard of this frame arrived
		XrTime last_shard = 0;
		// How often everything after the received shards was asked for (the frame's end didn't arrive)
		uint8_t tail_requests = 0;
		// Rebuilds missing data shards once there is enough parity: the first one rebuilt
		std::optional<uint16_t> recover();
		void reset(uint64_t frame_index);
		bool empty() const;

		std::optional<uint16_t> insert(data_shard &&, xr::instance & instance);

		wivrn::from_headset::feedback feedback{};

		explicit shard_set(uint8_t stream_index);
		shard_set(const shard_set &) = default;
		shard_set(shard_set &&) = default;
		shard_set & operator=(const shard_set &) = default;
		shard_set & operator=(shard_set &&) = default;

		uint64_t frame_index() const
		{
			return feedback.frame_index;
		}
	};

private:
	shard_set current;
	shard_set next;
	std::weak_ptr<scenes::stream> weak_scene;
	xr::instance & instance;

public:
	explicit shard_accumulator(
	        vk::raii::Device & device,
	        vk::raii::PhysicalDevice & physical_device,
	        xr::instance & instance,
	        uint32_t vk_queue_family_index,
	        const wivrn::to_headset::video_stream_description & description,
	        std::weak_ptr<scenes::stream> scene,
	        uint8_t stream_index) :
	        decoder_(decoder::make(device, physical_device, vk_queue_family_index, description, stream_index, scene, this)),
	        current(stream_index),
	        next(stream_index),
	        weak_scene(scene),
	        instance(instance)
	{
		next.reset(1);
	}

	void push_shard(wivrn::to_headset::video_stream_data_shard &&);
	void push_parity(wivrn::to_headset::video_stream_parity_shard &&);
	// Network thread, between packets: asks for what an incomplete frame lacks once its shards stopped arriving
	void request_stalled();

	vk::Sampler sampler()
	{
		return decoder_->sampler();
	}

	using blit_handle = decoder::blit_handle;

private:
	void request_missing(shard_set &, size_t limit, uint8_t max_requests = 1, bool tail = false);
	void try_submit_frame(std::optional<uint16_t> shard_idx);
	void try_submit_frame(uint16_t shard_idx);
	void send_feedback(wivrn::from_headset::feedback & feedback);
	void advance();
};
} // namespace wivrn
