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
#include "shard_stream.h"
#include "wivrn_packets.h"
#include <openxr/openxr.h>

#include <memory>
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

	shards::receiver recovery;
	std::map<uint64_t, from_headset::feedback> feedbacks;
	uint64_t next_frame = 0;
	uint8_t stream_index;
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
	        stream_index(stream_index),
	        weak_scene(scene),
	        instance(instance)
	{
	}

	void push_shard(wivrn::to_headset::video_shard &&);
	// Network thread, between packets: asks for what an incomplete frame lacks once its shards stopped arriving
	void request_stalled();

	vk::Sampler sampler()
	{
		return decoder_->sampler();
	}

	using blit_handle = decoder::blit_handle;

private:
	void request_missing(uint64_t frame, shards::receiver::frame &);
	void send_feedback(wivrn::from_headset::feedback & feedback);
};
} // namespace wivrn
