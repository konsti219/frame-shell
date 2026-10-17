// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

#include <cstdint>
#include <vector>

namespace wivrn
{
struct foveation_axis
{
	uint16_t encoded_size;
	uint16_t source_size;
	float center;

	bool operator==(const foveation_axis &) const = default;
};

struct foveation_description
{
	foveation_axis x, y;

	bool operator==(const foveation_description &) const = default;
};

struct foveation_parameter
{
	// Counts of encoded pixels per integer source-pixel ratio, with 1:1 at the middle entry.
	std::vector<uint16_t> x, y;
};

void generate_foveation_axis(const foveation_axis &, std::vector<uint16_t> &);
void generate_foveation(const foveation_description &, foveation_parameter &);
} // namespace wivrn
