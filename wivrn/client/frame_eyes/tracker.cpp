/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

// The camera loop and eye post-processing follow Babble Frame Runtime's DspEyeSource and BabbleEye (GPL-3.0-or-later)

#include "tracker.h"

#include "eye_gaze.h"

#include "camera/eye/dsp_client.h"
#include "camera/eye/eye_pairer.h"
#include "core/clock.h"
#include "model/executorch_runner.h"
#include "model/eye_kit.h"
#include "model/image_ops.h"
#include "model/postprocess/eye_post.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <spdlog/fmt/fmt.h>
#include <spdlog/spdlog.h>

#include <pthread.h>

using namespace std::chrono_literals;

namespace
{
// Control block bases seen in dsp_service builds
constexpr uint32_t ctrl_base_candidates[] = {0x234000, 0x124000};

// The eye kit's training crops of the 512x512 frames
struct crop
{
	uint32_t y, x, h, w;
};
constexpr crop crop_l{16, 56, 400, 400};
constexpr crop crop_r{16, 0, 400, 400};

constexpr uint64_t poll_ns = 400'000;
constexpr uint64_t no_frames_ns = 2'000'000'000;
constexpr uint64_t retry_min_ns = 5'000'000'000;
constexpr uint64_t retry_max_ns = 60'000'000'000;
constexpr uint32_t max_layout_faults = 3;

// Babble Frame Runtime 1.1's eye pipeline defaults (runtime/resources/defaults.json)
constexpr double ref_hz = 90;
constexpr double calib_warmup_s = 3;
constexpr double widen_seed = 0.15, widen_ceiling_max = 1.6, lid_saturation = 1.6, closed_seed = 0.3, calib_tol = 0.15;
constexpr double kit_widen_gain = 0.8;
constexpr uint32_t pupil_crop_px = 128;
constexpr double pupil_gate = 0.4, pupil_gate_hys = 0.1, pupil_hold_s = 0.1, pupil_rate_mm = 0.06, pupil_base_s = 60;
constexpr double pupil_gain = 2;

void sleep_ns(uint64_t ns)
{
	timespec ts{time_t(ns / 1'000'000'000), long(ns % 1'000'000'000)};
	nanosleep(&ts, nullptr);
}
} // namespace

struct frame_eyes::tracker::impl
{
	std::filesystem::path model_dir;
	uint32_t divisor;
	sender send;
	std::function<XrTime()> now;

	babble::DspClient client;
	babble::DspClient::Layout layout;
	babble::EyePairer pairer;
	eye_gaze gaze;

	babble::EyeKit kit;
	babble::EyeKit::Result result;
	std::vector<uint8_t> view_l, view_r, flipped;

	// Index 0 is the "left" ring (the wearer's right eye), 1 the "right" ring (the wearer's left eye)
	std::vector<babble::eye::OpennessCalibrator> openness;
	std::vector<babble::eye::PupilTracker> pupils;
	babble::eye::UpdateRate update_rate{ref_hz};
	babble::eye::RateScale tuned{ref_hz, ref_hz};
	uint64_t last_retune_ns = 0;
	uint64_t last_capture_ns = 0;

	bool load_model()
	{
		babble::ExecuTorchRunner::set_threads(1);
		if (not kit.load(model_dir / "eye_kit_stage1_fp16.pte", model_dir / "eye_kit_stage2_fp16.pte", pupil_crop_px))
			return false;
		view_l.resize(size_t(crop_l.w) * crop_l.h);
		view_r.resize(size_t(crop_r.w) * crop_r.h);
		flipped.resize(view_r.size());
		return true;
	}

	babble::eye::OpennessCalibrator::Params openness_params() const
	{
		babble::eye::OpennessCalibrator::Params p;
		p.skip = tuned.count(1.0 * tuned.ref_hz);
		p.warmup = tuned.count(calib_warmup_s * tuned.ref_hz);
		p.every = tuned.count(12);
		p.peak_window = tuned.count(9);
		p.trough_window = tuned.count(5);
		p.closed_seed = closed_seed;
		p.tol = calib_tol;
		p.widen_seed = widen_seed;
		p.ceiling_max = widen_ceiling_max;
		p.saturation = lid_saturation;
		return p;
	}

	void set_pupil_windows()
	{
		for (auto & t: pupils)
			t.set_base_window(tuned.count(pupil_base_s * tuned.ref_hz), tuned.count(tuned.ref_hz));
	}

	void start_session()
	{
		openness.clear();
		pupils.clear();
		for (int e = 0; e < 2; e++)
		{
			openness.emplace_back(openness_params(), babble::eye::kLidOneEuro);
			pupils.emplace_back(pupil_gate, pupil_gate_hys, int(tuned.count(pupil_hold_s * tuned.ref_hz)), babble::eye::kPupilOneEuroMm, pupil_rate_mm, 1, 1);
		}
		set_pupil_windows();
		last_capture_ns = 0;
		update_rate.restart();
	}

	void retune(double hz)
	{
		tuned.hz = hz;
		for (auto & t: pupils)
			t.set_hold(int(tuned.count(pupil_hold_s * tuned.ref_hz)));
		const auto p = openness_params();
		for (auto & c: openness)
			c.set_timing(p.skip, p.warmup, p.every, p.peak_window, p.trough_window);
		set_pupil_windows();
		spdlog::debug("Eyes: filters tuned for {:.0f} updates/s", hz);
	}

	bool copy_view(bool right, uint32_t idx, std::vector<uint8_t> & dst)
	{
		const uint8_t * src = client.slot_ptr(layout, right, idx);
		const crop & c = right ? crop_r : crop_l;
		if (src)
			for (uint32_t y = 0; y < c.h; y++)
				memcpy(dst.data() + size_t(y) * c.w, src + size_t(c.y + y) * layout.width + c.x, c.w);
		return src != nullptr;
	}

	bool run_kit()
	{
		babble::img::View views[2] = {
		        {view_l.data(), crop_l.w, crop_l.h, crop_l.w},
		        {view_r.data(), crop_r.w, crop_r.h, crop_r.w},
		};
		// For the right ring, rot90(2) and a mirror are the same as a vertical flip
		babble::img::flip_vertical(views[1], {flipped.data(), crop_r.w, crop_r.h, crop_r.w});
		views[1] = {flipped.data(), crop_r.w, crop_r.h, crop_r.w};
		return kit.run({views[0], views[1]}, result);
	}
};

frame_eyes::tracker::tracker(std::filesystem::path model_dir, uint32_t divisor, sender send, std::function<XrTime()> now) :
        d(std::make_unique<impl>())
{
	d->model_dir = std::move(model_dir);
	d->divisor = std::clamp<uint32_t>(divisor, 1, 6);
	d->send = std::move(send);
	d->now = std::move(now);
	thread = std::thread([this] { run(); });
}

frame_eyes::tracker::~tracker()
{
	quit = true;
	if (thread.joinable())
		thread.join();
}

void frame_eyes::tracker::set_present(bool p)
{
	if (p and not present.exchange(p))
		new_session = true;
	present = p;
}

std::string frame_eyes::tracker::statistics_json()
{
	std::lock_guard lock(stats_mutex);
	return fmt::format(R"({{"state":"{}","update_hz":{:.0f},"calibrated":{},"gaze":{}}})", stats.state, stats.update_hz, stats.calibrated, stats.gaze);
}

void frame_eyes::tracker::run()
{
	pthread_setname_np(pthread_self(), "frame_eyes");
	auto set_state = [&](const std::string & s) {
		std::lock_guard lock(stats_mutex);
		if (stats.state != s)
			spdlog::info("Eyes: {}", s);
		stats.state = s;
	};

	if (not d->load_model())
	{
		set_state("eye model missing or unusable in " + d->model_dir.string());
		return;
	}

	bool connected = false;
	uint64_t retry_at = 0, backoff = 0;
	uint64_t last_frame = 0, next_fps_check = 0, wait_ns = 5'000'000;
	uint32_t layout_faults = 0, pair_count = 0;
	uint64_t last_pair = 0;
	float camera_fps = 0;

	bool tracking = false;
	auto disconnect = [&](const std::string & why) {
		d->client.disconnect();
		connected = false;
		set_state(why);
		if (tracking)
		{
			// Tells the PC the eye data stopped
			d->send({.timestamp = d->now(), .gaze_valid = false, .lids_valid = false, .dilation = {-1, -1}});
			tracking = false;
		}
	};
	auto fail = [&](const std::string & why) {
		disconnect(why);
		backoff = backoff ? std::min(backoff * 2, retry_max_ns) : retry_min_ns;
		retry_at = babble::mono_ns() + backoff;
	};

	while (not quit)
	{
		if (not present)
		{
			if (connected)
				disconnect("paused while the headset is off");
			sleep_ns(200'000'000);
			continue;
		}
		if (new_session.exchange(false))
			d->start_session();

		uint64_t now = babble::mono_ns();
		if (not connected)
		{
			if (now < retry_at)
			{
				sleep_ns(100'000'000);
				continue;
			}
			std::string why;
			if (not d->client.connect(0))
			{
				fail("cannot connect to dsp_service");
				continue;
			}
			auto layout = d->client.probe(ctrl_base_candidates, &why);
			if (not layout)
			{
				bool unset = babble::DspClient::fps_unset(d->client.ctrl(), babble::DspClient::kRegionBytes, ctrl_base_candidates);
				fail(unset ? "waiting for dsp_service to set up the eye cameras" : "unrecognized dsp_service layout (" + why + ")");
				continue;
			}
			// Read only: Babble's start sequence and slot releases write the lock words Valve's own eye tracker
			// uses, which then times out waiting for images. The newest of 8 slots is copied long before it's reused.
			d->layout = *layout;
			d->pairer.reset();
			d->start_session();
			connected = true;
			backoff = 0;
			layout_faults = 0;
			last_frame = now;
			next_fps_check = 0;
			set_state("waiting for frames");
		}

		auto idx = d->client.read_indices(d->layout);
		now = babble::mono_ns();
		if (now >= next_fps_check)
		{
			// Valve's eye service sets the camera rate
			camera_fps = d->client.read_fps(d->layout);
			if (camera_fps > 0)
				wait_ns = uint64_t(0.5e9 / camera_fps);
			next_fps_check = now + 500'000'000;
			d->client.drain();
		}
		if ((idx.l != babble::DspClient::kNotReady and idx.l >= d->layout.slots) or
		    (idx.r != babble::DspClient::kNotReady and idx.r >= d->layout.slots))
		{
			if (++layout_faults >= max_layout_faults)
				fail("dsp_service index words out of range");
			else
				sleep_ns(poll_ns);
			continue;
		}
		layout_faults = 0;

		const auto step = d->pairer.feed(idx.l, idx.r, now, wait_ns);
		if (step.result == babble::EyePairer::Result::Half)
		{
			// A pair with an old image of one eye isn't usable
			last_frame = now;
			continue;
		}
		if (step.result != babble::EyePairer::Result::Pair)
		{
			if (now - last_frame > no_frames_ns)
				fail("no eye frames");
			else if (uint64_t interval = camera_fps > 0 ? uint64_t(1e9 / camera_fps) : 0;
			         not d->pairer.waiting() and interval and last_pair + interval > now + 1'500'000)
				// Sleep until shortly before the next frame
				sleep_ns(last_pair + interval - 1'500'000 - now);
			else
				sleep_ns(poll_ns);
			continue;
		}
		last_pair = now;
		last_frame = now;

		if (pair_count++ % d->divisor != 0)
			continue;
		bool ok_l = d->copy_view(false, idx.l, d->view_l);
		bool ok_r = d->copy_view(true, idx.r, d->view_r);
		if (not ok_l or not ok_r)
			continue;
		// The DSP gives no timestamps: the detection times' midpoint
		const uint64_t capture_ns = step.r_ns + uint64_t((int64_t(step.l_ns) - int64_t(step.r_ns)) / 2);

		d->update_rate.tick(capture_ns);
		const double dt = d->last_capture_ns and capture_ns > d->last_capture_ns ? double(capture_ns - d->last_capture_ns) / 1e9 : 0.0;
		d->last_capture_ns = capture_ns;
		{
			// Retune after a 10% change of the real update rate, at most once a second
			const double measured = d->update_rate.hz();
			const double hz = std::fabs(measured - d->tuned.ref_hz) < 0.03 * d->tuned.ref_hz ? d->tuned.ref_hz : measured;
			if (hz != d->tuned.hz and (hz == d->tuned.ref_hz or std::fabs(hz - d->tuned.hz) > 0.1 * d->tuned.hz) and
			    now - d->last_retune_ns > 1'000'000'000)
			{
				d->retune(hz);
				d->last_retune_ns = now;
			}
		}

		if (not d->run_kit())
			continue;

		std::array<double, 2> lid, widen;
		std::array<std::optional<double>, 2> dilation;
		for (int e = 0; e < 2; e++)
		{
			const auto r = d->openness[e].update(d->result.lid[e], dt);
			lid[e] = r.lid;
			// The kit's Widen head replaces the Widen from the openness
			widen[e] = d->kit.has_widen() ? std::clamp(d->result.widen[e] / kit_widen_gain, 0.0, 1.0) : r.widen;
			auto p = d->pupils[e].update(r.openness, d->result.mm[e], d->tuned.k(), dt);
			dilation[e] = babble::eye::dilation(p, pupil_gain);
		}
		// Both eyes get the mean, as Babble does by default; one eye's pupil until both have a baseline
		widen[0] = widen[1] = (widen[0] + widen[1]) / 2;
		if (dilation[0] and dilation[1])
			dilation[0] = dilation[1] = (*dilation[0] + *dilation[1]) / 2;
		else
			dilation[0] = dilation[1] = dilation[0] ? dilation[0] : dilation[1];

		std::array<XrVector3f, 2> gaze{};
		const bool gaze_valid = d->gaze.read(gaze);
		// Ring 1 is the wearer's left eye
		wivrn::from_headset::frame_eyes packet{
		        .timestamp = d->now() - XrTime(babble::mono_ns() - capture_ns),
		        .gaze = gaze,
		        .gaze_valid = gaze_valid,
		        .openness = {float(lid[1]), float(lid[0])},
		        .widen = {float(widen[1]), float(widen[0])},
		        .lids_valid = true,
		        .dilation = {float(dilation[1].value_or(-1)), float(dilation[0].value_or(-1))},
		};
		{
			std::lock_guard lock(stats_mutex);
			if (stats.state != "tracking")
				spdlog::info("Eyes: tracking");
			stats.state = "tracking";
			stats.update_hz = d->update_rate.hz();
			stats.calibrated = d->openness[0].ready() and d->openness[1].ready();
			stats.gaze = packet.gaze_valid;
		}
		d->send(std::move(packet));
		tracking = true;
	}
	d->client.disconnect();
}
