/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#pragma once

#include "wivrn_packets.h"

#include <cstring>
#include <deque>
#include <map>

// Audio over the stream socket instead of TCP, whose resends after a lost packet stall everything behind it (up to
// its 200 ms timeout) while the speaker buffer holds 20 ms. Chunks stay below the MTU, each is sent again
// `repeat_after` chunks later, and the receiver puts them back in order and drops duplicates.
namespace wivrn::audio_link
{
constexpr size_t max_chunk_bytes = 1152;
constexpr size_t repeat_after = 2;
// Chunks a gap may hold back before it's skipped
constexpr size_t max_waiting = 3;

class sender
{
	uint32_t sequence = 0;
	std::deque<audio_data> recent;

	static audio_data copy(const audio_data & data)
	{
		audio_data result{.timestamp = data.timestamp, .sequence = data.sequence};
		result.data.c = std::make_shared_for_overwrite<uint8_t[]>(data.payload.size());
		std::memcpy(result.data.c.get(), data.payload.data(), data.payload.size());
		result.payload = std::span(result.data.c.get(), data.payload.size());
		return result;
	}

public:
	// `send` gets each datagram to send; frame_size and frame_ns describe one sample frame of all channels
	template <typename F>
	void send(XrTime timestamp, std::span<const uint8_t> payload, size_t frame_size, double frame_ns, F && send)
	{
		const size_t chunk = std::max<size_t>(frame_size, max_chunk_bytes / std::max<size_t>(frame_size, 1) * std::max<size_t>(frame_size, 1));
		for (size_t offset = 0; offset < payload.size(); offset += chunk)
		{
			audio_data data{
			        .timestamp = timestamp + XrTime(offset / std::max<size_t>(frame_size, 1) * frame_ns),
			        .payload = std::span(const_cast<uint8_t *>(payload.data()) + offset, std::min(chunk, payload.size() - offset)),
			        .sequence = sequence++,
			};
			recent.push_back(copy(data));
			send(copy(data));
			if (recent.size() > repeat_after)
			{
				send(std::move(recent.front()));
				recent.pop_front();
			}
		}
	}
};

class receiver
{
	bool started = false;
	uint32_t next = 0;
	std::map<uint32_t, audio_data> waiting;

public:
	uint32_t skipped = 0, received = 0, duplicates = 0;

	// Calls `deliver` with each chunk in order, once
	template <typename F>
	void receive(audio_data && data, F && deliver)
	{
		if (not started or int32_t(data.sequence - next) < -1000)
		{
			// First chunk, or the sender started over
			started = true;
			next = data.sequence;
			waiting.clear();
		}
		++received;
		if (int32_t(data.sequence - next) < 0 or waiting.contains(data.sequence))
		{
			++duplicates;
			return;
		}
		waiting.emplace(data.sequence, std::move(data));
		while (not waiting.empty())
		{
			auto first = waiting.begin();
			if (first->first != next)
			{
				if (waiting.size() <= max_waiting)
					return;
				// Neither the chunk nor its repeat came: play on without it
				skipped += first->first - next;
				next = first->first;
			}
			deliver(std::move(first->second));
			waiting.erase(first);
			++next;
		}
	}
};
} // namespace wivrn::audio_link
