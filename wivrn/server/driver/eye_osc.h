/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#pragma once

#include "wivrn_packets.h"

#include <array>
#include <span>
#include <string>
#include <string_view>

namespace wivrn
{
// Sends the Steam Frame's eye data (from_headset::frame_eyes) to VRChat's native eye and face tracking OSC endpoints
class eye_osc
{
	// One Euro filter (https://gery.casiez.net/1euro/) state for one value
	struct one_euro
	{
		float x = 0, dx = 0;
		float step(float value, float dt);
	};

	int fd = -1;
	// Per-eye pitch and yaw, normalized so 45° is 1
	std::array<one_euro, 4> gaze_filter;
	XrTime last_gaze = 0;

	void send_message(const std::string & m);
	void send_floats(std::string_view address, std::span<const float> values);
	void send_blob(std::string_view address, std::span<const uint8_t> data);

public:
	// target is host:port
	explicit eye_osc(const std::string & target);
	eye_osc(const eye_osc &) = delete;
	eye_osc & operator=(const eye_osc &) = delete;
	~eye_osc();

	void send(const from_headset::frame_eyes &);
};
} // namespace wivrn
