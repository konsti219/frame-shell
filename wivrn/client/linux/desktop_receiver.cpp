// SPDX-License-Identifier: GPL-3.0-or-later
#include "desktop_receiver.h"
#include "application.h"
#include "fec.h"
#include "linux/frame_shell.h"
#include <algorithm>
#include <atomic>
#include <random>
#include <cstdio>
#include <spdlog/spdlog.h>
#if WIVRN_USE_V4L2
#include "decoder/v4l2/v4l2_decoder.h"
#endif

using namespace wivrn;

namespace frame_shell
{
// Testing (simulated_loss_percent): whether to drop this video shard, losing runs like Wi-Fi does
bool simulated_loss()
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

uint32_t next_source()
{
	static std::atomic<uint32_t> next = 1;
	return next++;
}

desktop_receiver::desktop_receiver(uint32_t source_, vk::raii::Device & device_, vk::raii::PhysicalDevice & physical_device_, uint32_t queue_family_index_, link sender_) :
        source(source_), device(device_), physical_device(physical_device_), queue_family_index(queue_family_index_), sender(std::move(sender_))
{
}

desktop_receiver::~desktop_receiver()
{
	// Decoders join threads that may be waiting for the lock in push_blit_handle
	streams_t old;
	{
		std::unique_lock lock(mutex);
		old.swap(streams);
	}
}

void desktop_receiver::operator()(to_headset::desktop_surface && surface)
{
#if WIVRN_USE_V4L2
	if (!enabled())
		return;
	desktop_surface_info info{
	        .source = source,
	        .surface = surface.surface,
	        .width = surface.width,
	        .height = surface.height,
	        .removed = surface.removed,
	};
	std::snprintf(info.name, sizeof(info.name), "%s", surface.name.c_str());
	std::snprintf(info.source_name, sizeof(info.source_name), "%s", surface.source.c_str());

	std::unique_ptr<desktop_stream> old;
	auto it = streams.find(surface.surface);
	if (it != streams.end() &&
	    (surface.removed || it->second->width != surface.width || it->second->height != surface.height ||
	     it->second->codec != surface.codec))
	{
		std::unique_lock lock(mutex);
		old = std::move(it->second);
		streams.erase(it);
		it = streams.end();
	}
	// Destroy outside the lock: the decoder joins threads that may be waiting for it in push_blit_handle
	old.reset();
	if (!surface.removed && it == streams.end())
	{
		auto stream = make_stream(surface.surface, surface.width, surface.height, surface.name, surface.codec);
		spdlog::info("Desktop surface {} ({} on {}): {}x{}", surface.surface, surface.name, surface.source, surface.width, surface.height);
		std::unique_lock lock(mutex);
		streams.emplace(surface.surface, std::move(stream));
	}
	desktop_surface(&info);
#endif
}

std::unique_ptr<desktop_receiver::desktop_stream> desktop_receiver::make_stream(uint32_t surface, uint16_t width, uint16_t height, const std::string & name, video_codec codec) noexcept
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
	        device, physical_device, queue_family_index, description, 0, weak_from_this(),
	        reinterpret_cast<shard_accumulator *>(stream.get()));
#endif
	return stream;
}

// A desktop video frame went missing: decoding resumes at the next keyframe, which the PC is asked for
void desktop_receiver::frame_lost(desktop_stream & stream)
{
	stream.wait_keyframe = true;
	auto now = std::chrono::steady_clock::now();
	if (now - stream.keyframe_requested < std::chrono::milliseconds(250))
		return;
	stream.keyframe_requested = now;
	try
	{
		sender.keyframe(from_headset::desktop_keyframe{.surface = stream.surface});
	}
	catch (...)
	{
	}
}

void desktop_receiver::operator()(to_headset::desktop_video && video)
{
#if WIVRN_USE_V4L2
	if (simulated_loss())
		return;
	auto it = streams.find(video.surface);
	if (it == streams.end() || !it->second->decoder)
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
	deliver_frames(it);
#endif
}

// Decodes a monitor's complete frames in order. Once a later frame arrives, an incomplete one asks for what it lacks
// (from_headset::desktop_nack) and later frames wait; if that doesn't come back in time it's given up and decoding
// resumes at the next keyframe.
void desktop_receiver::deliver_frames(streams_t::iterator it)
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
			if (not decode_frame(it, frame))
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
				sender.nack(std::move(nack));
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
		frame_lost(stream);
	}
}

// False if the decoder failed and the stream was replaced
bool desktop_receiver::decode_frame(streams_t::iterator it, desktop_stream::pending_frame & frame)
{
#if WIVRN_USE_V4L2
	auto & stream = *it->second;
	if (stream.wait_keyframe)
	{
		if (not wivrn::decoder::has_sps(frame.shards.front(), stream.codec))
		{
			frame_lost(stream);
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
			std::unique_lock lock(mutex);
			failed = std::move(it->second);
		}
		// Destroy outside the lock (the decoder joins its threads); its held frames go first
		auto surface = failed->surface;
		auto width = failed->width;
		auto height = failed->height;
		auto name = failed->name;
		auto codec = failed->codec;
		failed.reset();

		auto replacement = make_stream(surface, width, height, name, codec);
		replacement->failures = std::move(failures);
		frame_lost(*replacement);
		std::unique_lock lock(mutex);
		it->second = std::move(replacement);
		return false;
	}
#endif
	return true;
}

void desktop_receiver::push_blit_handle(shard_accumulator * decoder, std::shared_ptr<shard_accumulator::blit_handle> handle)
{
	std::shared_lock lock(mutex);
	auto it = std::ranges::find_if(streams, [&](auto & entry) {
		return reinterpret_cast<shard_accumulator *>(entry.second.get()) == decoder;
	});
	if (it == streams.end() or not it->second or handle->dmabuf_fd < 0)
		return;
	desktop_frame_info info{
	        .source = source,
	        .surface = it->second->surface,
	        .fd = handle->dmabuf_fd,
	        .width = handle->visible_extent.width,
	        .height = handle->visible_extent.height,
	        .stride = handle->dmabuf_stride,
	        .chroma_offset = handle->dmabuf_chroma_offset,
	        .id = handle->dmabuf_id,
	};
	present_desktop(std::move(handle), info);
}
} // namespace frame_shell
