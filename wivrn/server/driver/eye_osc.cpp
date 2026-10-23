/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "eye_osc.h"

#include "util/u_logging.h"

#include <algorithm>
#include <array>
#include <bit>
#include <cmath>
#include <cstring>
#include <netdb.h>
#include <stdexcept>
#include <sys/socket.h>
#include <unistd.h>

namespace
{
// Unified Expressions key order of /tracking/face/v1, one byte each
constexpr size_t face_key_count = 102;
constexpr size_t EyeLookOutRight = 0, EyeLookInRight = 1, EyeLookUpRight = 2, EyeLookDownRight = 3;
constexpr size_t EyeLookOutLeft = 4, EyeLookInLeft = 5, EyeLookUpLeft = 6, EyeLookDownLeft = 7;
constexpr size_t EyeClosedRight = 8, EyeClosedLeft = 9;
constexpr size_t EyeDilationRight = 10, EyeDilationLeft = 11, EyeConstrictRight = 12, EyeConstrictLeft = 13;
constexpr size_t EyeWideRight = 16, EyeWideLeft = 17;

// Gaze at or beyond this angle is a full EyeLook key and the native gaze cap
constexpr float max_gaze_deg = 45;

// Between Babble Frame Runtime's light and middle gaze presets, which are tuned on gaze normalized to max_gaze_deg
constexpr float gaze_min_cutoff = 2, gaze_beta = 0.4, gaze_d_cutoff = 1;
// A longer gap restarts the gaze filter from the raw value
constexpr float gaze_reset_s = 0.5;

void pad(std::string & s)
{
	s.push_back('\0');
	while (s.size() % 4)
		s.push_back('\0');
}

std::string message(std::string_view address, std::string_view tags)
{
	std::string m(address);
	pad(m);
	m += tags;
	pad(m);
	return m;
}

void append_be32(std::string & m, uint32_t value)
{
	value = __builtin_bswap32(value);
	m.append(reinterpret_cast<const char *>(&value), 4);
}

// Head-space direction (-Z forward) to VRChat's pitch (+ down) and yaw (+ right), in degrees
std::array<float, 2> pitch_yaw(const XrVector3f & v)
{
	constexpr float deg = 180 / M_PI;
	return {std::clamp(-std::atan2(v.y, -v.z) * deg, -max_gaze_deg, max_gaze_deg),
	        std::clamp(std::atan2(v.x, -v.z) * deg, -max_gaze_deg, max_gaze_deg)};
}

float lowpass_alpha(float cutoff, float dt)
{
	float r = 2 * M_PI * cutoff * dt;
	return r / (r + 1);
}

uint8_t to_byte(float value)
{
	return std::lround(std::clamp(value, 0.f, 1.f) * 255);
}
} // namespace

wivrn::eye_osc::eye_osc(const std::string & target)
{
	auto colon = target.rfind(':');
	if (colon == std::string::npos)
		throw std::runtime_error("eye-osc must be host:port, not " + target);
	std::string host = target.substr(0, colon), port = target.substr(colon + 1);
	addrinfo hints{.ai_socktype = SOCK_DGRAM};
	addrinfo * result = nullptr;
	if (int err = getaddrinfo(host.c_str(), port.c_str(), &hints, &result); err != 0)
		throw std::runtime_error("eye-osc " + target + ": " + gai_strerror(err));
	fd = socket(result->ai_family, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	if (fd < 0 or connect(fd, result->ai_addr, result->ai_addrlen) != 0)
	{
		freeaddrinfo(result);
		if (fd >= 0)
			close(fd);
		throw std::runtime_error("eye-osc " + target + ": " + strerror(errno));
	}
	freeaddrinfo(result);
	U_LOG_I("Eye data to %s via VRChat's native eye and face tracking", target.c_str());
}

wivrn::eye_osc::~eye_osc()
{
	close(fd);
}

float wivrn::eye_osc::one_euro::step(float value, float dt)
{
	float a_d = lowpass_alpha(gaze_d_cutoff, dt);
	dx = a_d * (value - x) / dt + (1 - a_d) * dx;
	float a = lowpass_alpha(gaze_min_cutoff + gaze_beta * std::abs(dx), dt);
	x = a * value + (1 - a) * x;
	return x;
}

void wivrn::eye_osc::send_message(const std::string & m)
{
	// Nobody listening (ECONNREFUSED) is fine
	(void)::send(fd, m.data(), m.size(), MSG_DONTWAIT | MSG_NOSIGNAL);
}

void wivrn::eye_osc::send_floats(std::string_view address, std::span<const float> values)
{
	std::string m = message(address, "," + std::string(values.size(), 'f'));
	for (float value: values)
		append_be32(m, std::bit_cast<uint32_t>(value));
	send_message(m);
}

void wivrn::eye_osc::send_blob(std::string_view address, std::span<const uint8_t> data)
{
	std::string m = message(address, ",b");
	append_be32(m, data.size());
	m.append(reinterpret_cast<const char *>(data.data()), data.size());
	while (m.size() % 4)
		m.push_back('\0');
	send_message(m);
}

void wivrn::eye_osc::send(const from_headset::frame_eyes & eyes)
{
	std::array<uint8_t, face_key_count> face{};

	if (eyes.gaze_valid)
	{
		auto [lp, ly] = pitch_yaw(eyes.gaze[0]);
		auto [rp, ry] = pitch_yaw(eyes.gaze[1]);
		std::array gaze{lp, ly, rp, ry};

		float dt = (eyes.timestamp - last_gaze) * 1e-9f;
		bool restart = last_gaze == 0 or dt > gaze_reset_s;
		for (size_t i = 0; i < gaze.size(); i++)
		{
			float v = gaze[i] / max_gaze_deg;
			if (restart)
				gaze_filter[i] = {.x = v};
			else if (dt > 0)
				gaze_filter[i].step(v, dt);
			gaze[i] = gaze_filter[i].x * max_gaze_deg;
		}
		if (restart or dt > 0)
			last_gaze = eyes.timestamp;
		send_floats("/tracking/eye/LeftRightPitchYaw", gaze);

		// Same gaze for avatars using Eye Gaze Mode: Face Tracking; yaw + is right, so out for the right eye
		auto look = [&](float angle) { return to_byte(angle / max_gaze_deg); };
		face[EyeLookOutRight] = look(gaze[3]);
		face[EyeLookInRight] = look(-gaze[3]);
		face[EyeLookUpRight] = look(-gaze[2]);
		face[EyeLookDownRight] = look(gaze[2]);
		face[EyeLookOutLeft] = look(-gaze[1]);
		face[EyeLookInLeft] = look(gaze[1]);
		face[EyeLookUpLeft] = look(-gaze[0]);
		face[EyeLookDownLeft] = look(gaze[0]);
	}
	else
		last_gaze = 0;

	if (eyes.lids_valid)
	{
		float closed_left = 1 - std::clamp(eyes.openness[0], 0.f, 1.f);
		float closed_right = 1 - std::clamp(eyes.openness[1], 0.f, 1.f);
		face[EyeClosedLeft] = to_byte(closed_left);
		face[EyeClosedRight] = to_byte(closed_right);
		face[EyeWideLeft] = to_byte(eyes.widen[0]);
		face[EyeWideRight] = to_byte(eyes.widen[1]);
		// Keeps VRChat's auto blink off for avatars without face tracking eyelids
		std::array closed{(closed_left + closed_right) / 2};
		send_floats("/tracking/eye/EyesClosedAmount", closed);
	}

	// 0.5 is the wearer's baseline, negative until there is one
	bool pupils = false;
	auto pupil = [&](float d, size_t dilation, size_t constrict) {
		if (d < 0)
			return;
		pupils = true;
		face[dilation] = to_byte((d - 0.5f) * 2);
		face[constrict] = to_byte((0.5f - d) * 2);
	};
	pupil(eyes.dilation[0], EyeDilationLeft, EyeConstrictLeft);
	pupil(eyes.dilation[1], EyeDilationRight, EyeConstrictRight);

	if (eyes.gaze_valid or eyes.lids_valid or pupils)
		send_blob("/tracking/face/v1", face);
}
