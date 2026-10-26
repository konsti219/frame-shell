/*
 * WiVRn VR streaming
 * Copyright (C) 2026 galister <galister-dev@pm.me>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

#pragma once

#include "decoder/decoder.h"
#include "wivrn_sockets.h"

#include <chrono>
#include <cstdint>
#include <deque>
#include <exception>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <stop_token>
#include <string>
#include <sys/mman.h>
#include <thread>
#include <vector>
#include <vulkan/vulkan_raii.hpp>

namespace scenes
{
class stream;
}

namespace wivrn
{
class shard_accumulator;
class frame_sink;
}

namespace wivrn::v4l2
{
class decoder : public wivrn::decoder
{
private:
	struct v4l2_blit_handle;

	struct mapped_plane
	{
		void * address = nullptr;
		size_t length = 0;
	};

	struct output_buffer
	{
		std::vector<mapped_plane> planes;
		~output_buffer()
		{
			for (auto & p: planes)
				if (p.address && p.address != MAP_FAILED)
					munmap(p.address, p.length);
		}
	};

	struct capture_buffer
	{
		std::vector<size_t> plane_lengths;
		uint64_t id = 0;
		vk::raii::DeviceMemory memory = nullptr;
		vk::raii::Image image = nullptr;
		vk::raii::ImageView image_view = nullptr;
		wivrn::fd_base dmabuf;
		vk::ImageLayout current_layout = vk::ImageLayout::eUndefined;
		bool held_by_vulkan = false;
	};

	struct pending_frame
	{
		uint64_t timestamp = 0;
		wivrn::from_headset::feedback feedback;
		wivrn::to_headset::video_frame_info::view_info_t view_info;
	};

	struct ready_output
	{
		uint32_t buffer_index;
		size_t bytes_used;
		pending_frame pending;
	};

	struct assembling_frame
	{
		uint32_t buffer_index;
		size_t bytes_used;
		uint64_t frame_index;
	};

	vk::raii::Device & device;
	vk::raii::PhysicalDevice & physical_device;
	// Shared with blit handles, which may outlive this decoder or a capture queue rebuild
	struct capture_set
	{
		vk::raii::SamplerYcbcrConversion ycbcr_conversion = nullptr;
		vk::raii::Sampler ycbcr_sampler = nullptr;
		std::vector<capture_buffer> buffers;
	};

	// Capture buffers released by blit handles, tagged with their capture set
	struct recycler
	{
		std::mutex mutex;
		std::deque<std::pair<const capture_set *, unsigned int>> queue;
		wivrn::fd_base wake_fd;

		void release(const capture_set * set, unsigned int index);
		void wake();
	};

	vk::Format capture_vk_format = vk::Format::eUndefined;

	wivrn::fd_base fd;
	std::vector<output_buffer> output_buffers;
	std::shared_ptr<capture_set> capture = std::make_shared<capture_set>();
	std::vector<unsigned int> free_output_buffers;
	std::deque<pending_frame> pending_frames;

	std::deque<ready_output> input_queue;
	std::mutex input_mutex;
	std::optional<assembling_frame> assembling;
	std::optional<uint64_t> dropped_frame_index;
	// Since when every OUTPUT buffer is queued in iris without one coming back; under output_mutex
	std::optional<std::chrono::steady_clock::time_point> output_starved_since;
	std::mutex output_mutex;
	std::deque<std::pair<const capture_set *, unsigned int>> recycle_capture_local;
	std::shared_ptr<recycler> recycle = std::make_shared<recycler>();
	std::jthread worker;
	std::exception_ptr worker_exception;
	std::mutex worker_exception_mutex;

	uint64_t next_timestamp = 1;
	unsigned int queued_output_count = 0;
	uint32_t capture_width = 0;
	uint32_t capture_height = 0;
	uint32_t capture_bytesperline = 0;
	vk::Extent2D capture_visible{};
	uint32_t capture_sizeimage = 0;
	uint32_t capture_pixelformat = 0;
	uint32_t capture_num_planes = 0;
	bool output_streaming = false;
	bool capture_streaming = false;

	std::weak_ptr<wivrn::frame_sink> weak_scene;
	shard_accumulator * accumulator;
	const vk::Extent2D extent;
	const wivrn::video_codec codec_type;
	const uint32_t vk_queue_family_index;

	void setup_device(wivrn::fd_base device_fd);
	void setup_output_queue();
	void setup_capture_queue();
	void teardown_capture_queue();
	void setup_ycbcr_sampler();
	void import_capture_buffer(unsigned int index, const std::vector<size_t> & plane_lengths);
	void initialize_capture_ownership();
	void queue_capture_buffer(unsigned int index);
	void recycle_capture_buffers();
	void handle_events();
	void reclaim_output_buffers();
	void drain_capture();
	void queue_ready_frames();
	void queue_output_buffer(const ready_output & ready);
	void abandon_assembling_frame();
	void worker_loop(std::stop_token stop_token);
	void wake_worker();
	void rethrow_worker_exception();
	void submit_capture(unsigned int capture_index, uint64_t timestamp);
	pending_frame take_pending(uint64_t timestamp);

public:
	decoder(vk::raii::Device & device,
	        vk::raii::PhysicalDevice & physical_device,
	        uint32_t vk_queue_family_index,
	        const wivrn::to_headset::video_stream_description & description,
	        uint8_t stream_index,
	        std::weak_ptr<wivrn::frame_sink> scene,
	        shard_accumulator * accumulator);
	~decoder() override;

	void push_data(std::span<std::span<const uint8_t>> data, uint64_t frame_index, bool partial) override;
	void frame_completed(
	        const wivrn::from_headset::feedback & feedback,
	        const wivrn::to_headset::video_frame_info::view_info_t & view_info) override;

	// Whether iris has taken no input for this long while all OUTPUT buffers are queued
	bool stalled(std::chrono::steady_clock::duration limit);

	vk::Sampler sampler() override
	{
		return *capture->ycbcr_sampler;
	}

	static bool available_for(wivrn::video_codec codec);
	static bool supports_10bit(wivrn::video_codec codec);
	static void supported_codecs(std::vector<wivrn::video_codec> & result);
};
} // namespace wivrn::v4l2
