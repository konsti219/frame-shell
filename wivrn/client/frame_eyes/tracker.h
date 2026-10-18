/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#pragma once

#include "wivrn_packets.h"

#include <atomic>
#include <filesystem>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>

namespace frame_eyes
{
// Lid openness, widen and pupil dilation from the Steam Frame's eye cameras (Babble's eye model on dsp_service
// frames), with per-eye gaze from Valve's eye tracker
class tracker
{
public:
	using sender = std::function<void(wivrn::from_headset::frame_eyes &&)>;

	// One model update per `divisor` camera frames
	tracker(std::filesystem::path model, uint32_t divisor, sender send, std::function<XrTime()> now);
	tracker(const tracker &) = delete;
	tracker & operator=(const tracker &) = delete;
	~tracker();

	// Pauses the cameras' reader and the model while the headset is off; putting it on starts a new calibration
	void set_present(bool present);
	std::string statistics_json();

	struct statistics
	{
		std::string state = "starting";
		float update_hz = 0;
		bool calibrated = false;
		bool gaze = false;
	};

private:
	struct impl;
	std::unique_ptr<impl> d;
	std::thread thread;
	std::atomic<bool> quit{false};
	std::atomic<bool> present{true};
	std::atomic<bool> new_session{false};
	std::mutex stats_mutex;
	statistics stats;

	void run();
};
} // namespace frame_eyes
