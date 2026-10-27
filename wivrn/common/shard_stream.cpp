// SPDX-License-Identifier: GPL-3.0-or-later
#include "shard_stream.h"

#include "fec.h"
#include <algorithm>
#include <limits>
#include <stdexcept>

namespace wivrn::shards
{
std::vector<to_headset::video_shard> sender::frame(uint32_t stream, uint32_t generation, uint64_t frame, std::span<const uint8_t> data, uint8_t fec_percent)
{
	constexpr size_t shard_size = to_headset::video_shard::max_payload_size;
	const size_t count = (data.size() + shard_size - 1) / shard_size;
	if (count == 0 or count > std::numeric_limits<uint16_t>::max())
		throw std::logic_error("Invalid video frame size");
	const uint16_t parity_count = fec::parity_count(count, fec_percent);
	if (count + parity_count > std::numeric_limits<uint16_t>::max())
		throw std::logic_error("Video frame exceeds the shard index range");

	std::vector<to_headset::video_shard> shards;
	shards.reserve(count + parity_count);
	auto shard = [&](uint16_t index, std::vector<uint8_t> payload) {
		shards.push_back({
		        .stream = stream,
		        .generation = generation,
		        .frame = frame,
		        .index = index,
		        .data_count = uint16_t(count),
		        .parity_count = parity_count,
		        .frame_size = uint32_t(data.size()),
		        .payload = std::move(payload),
		});
	};
	std::vector<std::vector<uint8_t>> blocks;
	for (size_t i = 0; i < count; ++i)
	{
		auto begin = data.begin() + std::min(data.size(), i * shard_size);
		auto end = data.begin() + std::min(data.size(), (i + 1) * shard_size);
		std::vector<uint8_t> payload(begin, end);
		// Parity is over equal blocks
		if (parity_count)
		{
			payload.resize(fec::block_size);
			blocks.push_back(payload);
		}
		shard(i, std::move(payload));
	}
	auto parity = fec::encode(blocks, parity_count);
	for (size_t i = 0; i < parity.size(); ++i)
		shard(count + i, std::move(parity[i]));

	auto & kept = sent[frame % sent.size()];
	kept.frame = frame;
	// Sending encrypts payloads in place: what's sent is a copy
	kept.shards = shards;
	return shards;
}

std::vector<to_headset::video_shard> sender::resend(const from_headset::video_nack & nack) const
{
	const auto & kept = sent[nack.frame % sent.size()];
	if (kept.frame != nack.frame or kept.shards.empty() or
	    kept.shards.front().stream != nack.stream or kept.shards.front().generation != nack.generation)
		return {};
	std::vector<to_headset::video_shard> shards;
	for (auto index: nack.shards)
		if (index < kept.shards.size())
			shards.push_back(kept.shards[index]);
	return shards;
}

std::span<const uint8_t> receiver::frame::payload(size_t i) const
{
	size_t offset = i * to_headset::video_shard::max_payload_size;
	return std::span(data[i]).first(std::min<size_t>(data[i].size(), size - std::min<size_t>(size, offset)));
}

bool receiver::frame::resent() const
{
	return std::ranges::any_of(requested, [](uint8_t count) { return count > 0; });
}

receiver::frame * receiver::add(to_headset::video_shard && shard)
{
	constexpr size_t shard_size = to_headset::video_shard::max_payload_size;
	if (shard.data_count == 0 or shard.parity_count > shard.data_count or
	    uint32_t(shard.data_count) + shard.parity_count > std::numeric_limits<uint16_t>::max() or
	    shard.index >= uint32_t(shard.data_count) + shard.parity_count or
	    shard.frame_size > size_t(shard.data_count) * shard_size or
	    shard.frame_size <= size_t(shard.data_count - 1) * shard_size)
		return nullptr;
	const size_t expected_size = shard.parity_count or shard.index + 1 < shard.data_count
	                                     ? shard_size
	                                     : shard.frame_size - size_t(shard.data_count - 1) * shard_size;
	if (shard.payload.size() != expected_size)
		return nullptr;

	auto & f = frames[shard.frame];
	auto now = clock::now();
	if (f.data.empty())
	{
		f.size = shard.frame_size;
		f.data_count = shard.data_count;
		f.parity_count = shard.parity_count;
		f.data.resize(shard.data_count);
		f.parity.resize(shard.parity_count);
		f.requested.resize(shard.data_count);
	}
	if (shard.frame_size != f.size or shard.data_count != f.data_count or shard.parity_count != f.parity_count)
		return nullptr;
	f.last_shard = now;
	if (f.complete)
		return &f;
	if (shard.index < f.data_count)
	{
		if (f.data[shard.index].empty() and not shard.payload.empty())
		{
			f.data[shard.index] = std::move(shard.payload);
			++f.received;
		}
	}
	else if (auto & parity = f.parity[shard.index - f.data_count]; parity.empty())
	{
		parity = std::move(shard.payload);
		++f.parity_received;
	}

	if (f.received < f.data_count and f.received + f.parity_received >= f.data_count)
	{
		size_t missing = f.data_count - f.received;
		if (fec::decode(f.data, f.parity))
		{
			f.recovered = missing;
			f.received = f.data_count;
		}
	}
	f.complete = f.received == f.data_count;
	return &f;
}

std::vector<uint16_t> receiver::missing(frame & f, uint8_t max_requests)
{
	std::vector<uint16_t> missing;
	for (size_t i = 0; i < f.data.size(); ++i)
		if (f.data[i].empty() and f.requested[i] < max_requests)
		{
			missing.push_back(i);
			++f.requested[i];
		}
	if (not missing.empty())
		f.last_request = clock::now();
	return missing;
}
} // namespace wivrn::shards
