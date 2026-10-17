/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#include "fec.h"

#include "leopard.h"
#include "wivrn_serialization.h"
#include <algorithm>
#include <cstring>
#include <cstdlib>
#include <memory>
#include <mutex>
#include <stdexcept>

namespace wivrn::fec
{
namespace
{
void init()
{
	static std::once_flag once;
	std::call_once(once, [] {
		if (leo_init() != 0)
			throw std::runtime_error("Leopard erasure coding is unavailable");
	});
}

// Leopard's SIMD paths want aligned buffers
struct aligned_blocks
{
	std::unique_ptr<uint8_t[], decltype(&free)> memory{nullptr, &free};
	std::vector<void *> pointers;

	explicit aligned_blocks(size_t count)
	{
		memory.reset((uint8_t *)aligned_alloc(64, count * block_size));
		for (size_t i = 0; i < count; ++i)
			pointers.push_back(memory.get() + i * block_size);
	}
};
} // namespace

uint16_t parity_count(size_t data_count, uint8_t percent)
{
	if (percent == 0 or data_count == 0)
		return 0;
	return std::min<size_t>(data_count, std::max<size_t>(1, (data_count * percent + 99) / 100));
}

std::vector<uint8_t> block(const to_headset::video_stream_data_shard & shard)
{
	serialization_packet packet;
	packet.serialize(shard);
	std::vector<uint8_t> result(block_size, 0);
	size_t size = 2;
	for (const auto & span: (std::vector<std::span<uint8_t>> &)packet)
	{
		if (size + span.size() > block_size)
			throw std::logic_error("Video shard exceeds the FEC block size");
		memcpy(result.data() + size, span.data(), span.size());
		size += span.size();
	}
	uint16_t length = size - 2;
	memcpy(result.data(), &length, 2);
	return result;
}

std::optional<to_headset::video_stream_data_shard> shard(std::span<const uint8_t> block)
{
	if (block.size() < 2)
		return std::nullopt;
	uint16_t length;
	memcpy(&length, block.data(), 2);
	if (length + 2u > block.size())
		return std::nullopt;
	std::shared_ptr<uint8_t[]> memory(new uint8_t[length]);
	memcpy(memory.get(), block.data() + 2, length);
	try
	{
		deserialization_packet packet(memory, {memory.get(), length});
		return packet.deserialize<to_headset::video_stream_data_shard>();
	}
	catch (std::exception &)
	{
		return std::nullopt;
	}
}

std::vector<std::vector<uint8_t>> encode(std::span<const std::vector<uint8_t>> data, uint16_t parity_count)
{
	init();
	if (parity_count == 0 or data.empty())
		return {};
	std::vector<const void *> originals;
	for (const auto & block: data)
		originals.push_back(block.data());
	unsigned work_count = leo_encode_work_count(data.size(), parity_count);
	aligned_blocks work(work_count);
	if (leo_encode(block_size, data.size(), parity_count, work_count, originals.data(), work.pointers.data()) != Leopard_Success)
		throw std::logic_error("Invalid Leopard encoder configuration");
	std::vector<std::vector<uint8_t>> parity;
	for (unsigned i = 0; i < parity_count; ++i)
		parity.emplace_back((uint8_t *)work.pointers[i], (uint8_t *)work.pointers[i] + block_size);
	return parity;
}

bool decode(std::vector<std::vector<uint8_t>> & data, std::span<const std::vector<uint8_t>> parity)
{
	init();
	size_t missing = std::ranges::count_if(data, [](const auto & block) { return block.empty(); });
	size_t recovery = std::ranges::count_if(parity, [](const auto & block) { return not block.empty(); });
	if (missing == 0)
		return true;
	if (recovery < missing)
		return false;
	std::vector<const void *> originals;
	for (const auto & block: data)
		originals.push_back(block.empty() ? nullptr : block.data());
	std::vector<const void *> recoveries;
	for (const auto & block: parity)
		recoveries.push_back(block.empty() ? nullptr : block.data());
	unsigned work_count = leo_decode_work_count(data.size(), parity.size());
	aligned_blocks work(work_count);
	if (leo_decode(block_size, data.size(), parity.size(), work_count, originals.data(), recoveries.data(), work.pointers.data()) != Leopard_Success)
		return false;
	for (size_t i = 0; i < data.size(); ++i)
		if (data[i].empty())
			data[i].assign((uint8_t *)work.pointers[i], (uint8_t *)work.pointers[i] + block_size);
	return true;
}
} // namespace wivrn::fec
