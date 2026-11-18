// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <algorithm>
#include <cstdint>

namespace wivrn
{
// A capacity of 0 is no estimate, which limits nothing
inline uint32_t adapted_video_bitrate(uint32_t desired, uint32_t current, uint64_t capacity)
{
	const uint32_t target = capacity ? std::min<uint64_t>(desired, std::max<uint64_t>(1'000'000, capacity * 0.85)) : desired;
	if (target < current * 0.9)
		return target;
	if (target > current * 1.1 or (target == desired and target > current))
		return std::min<uint32_t>(target, current * 1.1);
	return current;
}
} // namespace wivrn
