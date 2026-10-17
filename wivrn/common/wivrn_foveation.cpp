/*
 * WiVRn VR streaming
 * Copyright (C) 2024  galister <galister@librevr.org>
 * Copyright (C) 2025  Patrick Nicolas <patricknicolas@laposte.net>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#include "wivrn_foveation.h"

#include <algorithm>
#include <cassert>
#include <cmath>
#include <optional>
#include <tuple>

namespace
{
// a, b: parameters computed by solve_foveation
// λ: pixel ratio between full size and foveated image (in range ]0,1[)
// c: coordinates in -1,1 range of the full size image where pixel ratio must be 1:1
// x: coordinates in -1,1 range of the foveated image
// result: full size image coordinates in -1,1 range
static double defoveate(double a, double b, double λ, double c, double x)
{
	// In order to save encoding, transmit and decoding time, only a portion of the image is encoded in full resolution.
	// on each axis, foveated coordinates are defined by the following formula.
	return λ / a * tan(a * x + b) + c;
	// a and b are defined such as:
	// edges of the image are not moved
	// f(-1) = -1
	// f( 1) =  1
	// the function also enforces pixel ratio 1:1 at fovea
	// df⁻¹(x)/dx = 1/scale for x = c

	// We then ensure that source and destination pixel grids match by
	// rounding to integer pixel ratios: 1:1, 1:2 etc.
	// Finally, pixel spans are sorted so that we only have increasing
	// ratios going out from the center.
}

static std::tuple<float, float> solve_foveation(float λ, float c)
{
	// Compute a and b for the foveation function such that:
	//   foveate(a, b, scale, c, -1) = -1   (eq. 1)
	//   foveate(a, b, scale, c,  1) =  1   (eq. 2)
	//
	// Use eq. 2 to express a as function of b, then replace in eq. 1
	// equation that needs to be null is:
	auto b = [λ, c](double a) { return atan(a * (1 - c) / λ) - a; };
	auto eq = [λ, c](double a) { return atan(a * (1 - c) / λ) + atan(a * (1 + c) / λ) - 2 * a; }; // (eq. 3)

	// function starts positive, reaches a maximum then decreases to -∞
	double a0 = 0;
	// Find a negative value by computing eq(2^n)
	double a1 = 1;
	while (eq(a1) > 0)
		a1 *= 2;

	// last computed values for f(a0) and f(a1)
	std::optional<double> f_a0;
	double f_a1 = eq(a1);

	int n = 0;
	double a = 0;
	while (std::abs(a1 - a0) > 0.0000001 && n++ < 100)
	{
		if (not f_a0)
		{
			// use binary search
			a = 0.5 * (a0 + a1);
			double val = eq(a);
			if (val > 0)
			{
				a0 = a;
				f_a0 = val;
			}
			else
			{
				a1 = a;
				f_a1 = val;
			}
		}
		else
		{
			// f(a1) is always defined
			// when f(a0) is defined, use secant method
			a = a1 - f_a1 * (a1 - a0) / (f_a1 - *f_a0);
			a0 = a1;
			a1 = a;
			f_a0 = f_a1;
			f_a1 = eq(a);
		}
	}

	return {a, b(a)};
}

} // namespace

void wivrn::generate_foveation_axis(const foveation_axis & axis, std::vector<uint16_t> & out)
{
	const auto [foveated_dim, source_dim, c] = axis;
	if (foveated_dim >= source_dim)
	{
		out.assign(1, source_dim);
		return;
	}
	float scale = float(foveated_dim) / source_dim;
	auto [a, b] = solve_foveation(scale, c);

	uint16_t last = 0;
	std::vector<uint16_t> left; // index 0: 1:1 ratio, then 2:1 etc.
	std::vector<uint16_t> right;
	for (size_t i = 1; i < foveated_dim; ++i)
	{
		double u = (i * 2.) / foveated_dim - 1;
		auto f = defoveate(a, b, scale, c, u);
		uint16_t n = std::clamp<uint16_t>((f * 0.5 + 0.5) * source_dim + 0.5, 0, source_dim);
		assert(n > last);
		size_t count = n - last;
		auto & vec = u < c ? left : right;
		if (count > vec.size())
			vec.resize(count);
		vec[count - 1]++;
		last = n;
	}
	assert(last < source_dim);
	size_t count = source_dim - last;
	if (count > right.size())
		right.resize(count);
	right[count - 1]++;

	count = std::max(left.size(), right.size());
	out.clear();
	out.resize(count - left.size());
	out.insert(out.end(), left.rbegin(), left.rend());
	if (not right.empty())
		out.back() += right.front();
	if (right.size() > 1)
		out.insert(out.end(), right.begin() + 1, right.end());
	out.resize(count * 2 - 1);
}

void wivrn::generate_foveation(const foveation_description & description, foveation_parameter & out)
{
	generate_foveation_axis(description.x, out.x);
	generate_foveation_axis(description.y, out.y);
}
