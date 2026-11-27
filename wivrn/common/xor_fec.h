// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <algorithm>
#include <array>
#include <cstdint>
#include <map>
#include <optional>
#include <span>
#include <vector>

namespace wivrn
{
struct fec_packet
{
	uint64_t sequence;
	std::vector<uint8_t> bytes;
};

inline std::vector<uint8_t> fec_encode(std::span<const fec_packet> packets)
{
	if (packets.size() < 2 or packets.size() > 4)
		return {};
	std::vector<uint8_t> result{uint8_t(packets.size())};
	size_t width = 0;
	for (const auto & p: packets)
	{
		if (p.bytes.empty() or p.bytes.size() > 1121)
			return {};
		for (size_t i = 0; i < 8; ++i)
			result.push_back(p.sequence >> (i * 8));
		result.push_back(p.bytes.size() & 255);
		result.push_back(p.bytes.size() >> 8);
		width = std::max(width, p.bytes.size());
	}
	const size_t offset = result.size();
	result.resize(offset + width);
	for (const auto & p: packets)
		for (size_t i = 0; i < p.bytes.size(); ++i)
			result[offset + i] ^= p.bytes[i];
	return result;
}

inline std::optional<fec_packet> fec_recover(std::span<const uint8_t> parity, const std::map<uint64_t, std::vector<uint8_t>> & received, bool * finished = nullptr)
{
	if (finished)
		*finished = true;
	if (parity.empty() or parity[0] < 2 or parity[0] > 4)
		return {};
	const size_t count = parity[0], offset = 1 + count * 10;
	if (parity.size() <= offset or parity.size() - offset > 1121)
		return {};
	std::array<std::pair<uint64_t, size_t>, 4> entries{};
	size_t missing_count = 0, missing_index = 0;
	for (size_t n = 0; n < count; ++n)
	{
		const auto item = parity.subspan(1 + n * 10, 10);
		uint64_t sequence = 0;
		for (size_t i = 0; i < 8; ++i)
			sequence |= uint64_t(item[i]) << (i * 8);
		const size_t size = item[8] | (size_t(item[9]) << 8);
		if (not sequence or not size or size > parity.size() - offset)
			return {};
		for (size_t i = 0; i < n; ++i)
			if (entries[i].first == sequence)
				return {};
		entries[n] = {sequence, size};
		const auto found = received.find(sequence);
		if (found == received.end())
		{
			++missing_count;
			missing_index = n;
		}
		else if (found->second.size() != size)
			return {};
	}
	if (missing_count != 1)
	{
		if (finished)
			*finished = missing_count == 0;
		return {};
	}
	std::vector<uint8_t> bytes(parity.begin() + offset, parity.end());
	for (size_t n = 0; n < count; ++n)
		if (n != missing_index)
			for (size_t i = 0; i < entries[n].second; ++i)
				bytes[i] ^= received.at(entries[n].first)[i];
	bytes.resize(entries[missing_index].second);
	return fec_packet{entries[missing_index].first, std::move(bytes)};
}
} // namespace wivrn
