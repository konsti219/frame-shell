/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <openxr/openxr.h>

namespace frame_eyes
{
// Per-eye gaze from Valve's eye tracker (/dev/shm/eye-server.mmap, layout versions 4 and 5), with the frame shell's
// click calibration (~/.config/monado/eye-calibration) applied as in frame-monado's reader
class eye_gaze
{
	uint8_t * base = nullptr;
	size_t map_size = 0;
	size_t record = 0;
	uint32_t last_sequence = 0;
	uint64_t next_open_ns = 0;
	uint64_t received_ns = 0;
	std::array<XrVector3f, 2> latest{};

	bool open(uint64_t now_ns);

public:
	eye_gaze() = default;
	eye_gaze(const eye_gaze &) = delete;
	eye_gaze & operator=(const eye_gaze &) = delete;
	~eye_gaze();

	// Left and right eye of the wearer, head space (-Z forward); false without a sample in the last 200 ms
	bool read(std::array<XrVector3f, 2> & out);
};
} // namespace frame_eyes
