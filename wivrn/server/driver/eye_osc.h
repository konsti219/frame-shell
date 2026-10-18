/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#pragma once

#include "wivrn_packets.h"

#include <chrono>
#include <string>
#include <string_view>

namespace wivrn
{
// Sends the Steam Frame's eye data (from_headset::frame_eyes) as VRCFT v2 avatar parameters over OSC
class eye_osc
{
	int fd = -1;
	std::string prefix;
	bool active = false;
	std::chrono::steady_clock::time_point last_active{};

	void send_float(std::string_view name, float value);
	void send_active(bool value);

public:
	// target is host:port, prefix the avatar parameter prefix (e.g. "/FT")
	eye_osc(const std::string & target, std::string prefix);
	eye_osc(const eye_osc &) = delete;
	eye_osc & operator=(const eye_osc &) = delete;
	~eye_osc();

	void send(const from_headset::frame_eyes &);
};
} // namespace wivrn
