#pragma once

// SPDX-License-Identifier: GPL-3.0-or-later

#include "frame_shell.h"
#include <array>

namespace frame_shell
{
class focus_reader
{
	std::array<bool, 256> held{};
	std::array<wivrn::interaction_profile, 3> profiles{};
	std::array<float, 256> last_value{};
	std::array<XrTime, 256> last_change{};

public:
	focus_reader() = default;
	focus_reader(const focus_reader &) = delete;
	focus_reader & operator=(const focus_reader &) = delete;

	void filter(wivrn::from_headset::inputs & inputs, XrTime now, const std::array<wivrn::interaction_profile, 3> & current_profiles)
	{
		if (profiles != current_profiles)
		{
			held = {};
			profiles = current_profiles;
		}
		auto current = input_capture_snapshot();
		std::array<bool, 256> active{}, captured{};
		for (const auto & input: inputs.values)
		{
			auto id = static_cast<uint8_t>(input.id);
			auto group = wivrn::input_groups[id];
			active[group] |= input.value != 0;
			captured[group] |= current.inputs[id];
		}
		for (size_t group = 0; group < held.size(); ++group)
			held[group] = active[group] && (held[group] || captured[group]);

		for (auto & input: inputs.values)
		{
			auto index = static_cast<uint8_t>(input.id);
			auto group = wivrn::input_groups[index];
			bool suppress = captured[group] || held[group];
			float value = suppress ? 0 : input.value;
			if (value != last_value[index])
				last_change[index] = now;
			input.value = last_value[index] = value;
			input.last_change_time = last_change[index];
		}
	}
};
} // namespace frame_shell
