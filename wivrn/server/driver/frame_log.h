// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

#include "wivrn_packets.h"

#include <cstdint>
#include <fstream>
#include <mutex>

namespace wivrn
{

// JSON lines for streaming tests on real hardware, written while WIVRN_FRAME_LOG names a file.
// Times are on the headset clock.
class frame_log
{
	std::mutex mutex;
	std::ofstream file;

	void write(const std::string & line);

public:
	frame_log();

	explicit operator bool() const
	{
		return file.is_open();
	}

	void sent(uint8_t stream, uint64_t frame, const to_headset::video_frame_info::timing_info_t &, size_t bytes);
	void feedback(const from_headset::feedback &);
	void network(XrTime now, const to_headset::network_report &);
};

} // namespace wivrn
