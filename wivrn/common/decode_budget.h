// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>

namespace wivrn
{
class decode_budget
{
	std::array<int64_t, 64> samples{};
	size_t count = 0, cursor = 0;
	uint64_t generation = 0, frame = 0;

public:
	int64_t observe(uint64_t stream_generation, uint64_t frame_index, int64_t duration_ns)
	{
		if (stream_generation != generation)
		{
			generation = stream_generation;
			count = cursor = 0;
		}
		if ((count and frame_index <= frame) or duration_ns <= 0 or duration_ns > 100'000'000)
			return estimate();
		frame = frame_index;
		samples[cursor++ % samples.size()] = duration_ns;
		count = std::min(count + 1, samples.size());
		return estimate();
	}

	int64_t estimate() const
	{
		if (not count)
			return 2'000'000;
		auto values = samples;
		const size_t percentile = (count - 1) * 95 / 100;
		std::nth_element(values.begin(), values.begin() + percentile, values.begin() + count);
		return std::clamp<int64_t>(values[percentile] + 1'000'000, 2'000'000, 20'000'000);
	}
};
} // namespace wivrn
