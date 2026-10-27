// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

// Per-stream video sharding, FEC and retransmission; callers provide synchronization.

#include "wivrn_packets.h"
#include <array>
#include <chrono>
#include <cstdint>
#include <map>
#include <span>
#include <vector>

namespace wivrn::shards
{
class sender
{
	struct sent_frame
	{
		uint64_t frame = -1;
		std::vector<to_headset::video_shard> shards;
	};
	std::array<sent_frame, 8> sent;
	uint64_t last = -1;

public:
	// A frame's shards to send, data then parity (fec_percent of the data shards, 0: none); kept to send again
	std::vector<to_headset::video_shard> frame(uint32_t stream, uint32_t generation, uint64_t frame, std::span<const uint8_t> data, uint8_t fec_percent);
	// What a resend request asks for, while the frame is kept
	std::vector<to_headset::video_shard> resend(const from_headset::video_nack &) const;
};

class receiver
{
public:
	using clock = std::chrono::steady_clock;
	struct frame
	{
		uint32_t size = 0;
		uint16_t data_count = 0, parity_count = 0;
		// Not received yet: empty
		std::vector<std::vector<uint8_t>> data, parity;
		size_t received = 0, parity_received = 0;
		bool complete = false;
		// Data shards rebuilt from parity
		uint16_t recovered = 0;
		// How often each data shard was asked for again, and when last
		std::vector<uint8_t> requested;
		clock::time_point last_shard, last_request;

		// Data shard i's part of the frame (without the padding parity needs)
		std::span<const uint8_t> payload(size_t i) const;
		bool resent() const;
	};
	// Frames still arriving or waiting to be used, by number
	std::map<uint64_t, frame> frames;

	// Adds a shard to its frame; that frame, or nullptr for a shard that doesn't fit what its frame announced
	frame * add(to_headset::video_shard &&);
	// Missing data shards, counted as requested
	std::vector<uint16_t> missing(frame &, uint8_t max_requests);
};
} // namespace wivrn::shards
