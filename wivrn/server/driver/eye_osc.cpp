/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "eye_osc.h"

#include "util/u_logging.h"

#include <algorithm>
#include <bit>
#include <cmath>
#include <cstring>
#include <netdb.h>
#include <stdexcept>
#include <sys/socket.h>
#include <unistd.h>

namespace
{
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

// atan2 per axis scaled so 45° is 1, as frameeyeosc sends them
std::array<float, 2> gaze_angles(const XrVector3f & v)
{
	constexpr float scale = 4 / M_PI;
	return {std::clamp(std::atan2(v.x, -v.z) * scale, -1.f, 1.f), std::clamp(std::atan2(v.y, -v.z) * scale, -1.f, 1.f)};
}
} // namespace

wivrn::eye_osc::eye_osc(const std::string & target, std::string prefix) :
        prefix("/avatar/parameters" + prefix)
{
	while (this->prefix.ends_with('/'))
		this->prefix.pop_back();
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
	U_LOG_I("Eye data to %s as VRCFT parameters under %s", target.c_str(), this->prefix.c_str());
}

wivrn::eye_osc::~eye_osc()
{
	if (active)
		send_active(false);
	close(fd);
}

void wivrn::eye_osc::send_float(std::string_view name, float value)
{
	std::string m = message(prefix + "/v2/" + std::string(name), ",f");
	uint32_t bits = std::bit_cast<uint32_t>(value);
	bits = __builtin_bswap32(bits);
	m.append(reinterpret_cast<const char *>(&bits), 4);
	// Nobody listening (ECONNREFUSED) is fine
	(void)::send(fd, m.data(), m.size(), MSG_DONTWAIT | MSG_NOSIGNAL);
}

void wivrn::eye_osc::send_active(bool value)
{
	std::string m = message(prefix + "/EyeTrackingActive", value ? ",T" : ",F");
	(void)::send(fd, m.data(), m.size(), MSG_DONTWAIT | MSG_NOSIGNAL);
	active = value;
	last_active = std::chrono::steady_clock::now();
}

void wivrn::eye_osc::send(const from_headset::frame_eyes & eyes)
{
	bool tracking = eyes.gaze_valid or eyes.lids_valid;
	// Repeated once a second, so an avatar loaded later picks it up
	if (tracking != active or (tracking and std::chrono::steady_clock::now() - last_active > std::chrono::seconds(1)))
		send_active(tracking);

	if (eyes.gaze_valid)
	{
		auto [lx, ly] = gaze_angles(eyes.gaze[0]);
		auto [rx, ry] = gaze_angles(eyes.gaze[1]);
		XrVector3f sum{eyes.gaze[0].x + eyes.gaze[1].x, eyes.gaze[0].y + eyes.gaze[1].y, eyes.gaze[0].z + eyes.gaze[1].z};
		auto [x, y] = gaze_angles(sum);
		send_float("EyeLeftX", lx);
		send_float("EyeLeftY", ly);
		send_float("EyeRightX", rx);
		send_float("EyeRightY", ry);
		send_float("EyeX", x);
		send_float("EyeY", y);
	}
	if (eyes.lids_valid)
	{
		// 0..0.75 is openness, 0.75..1 widen
		auto lid = [&](int e) { return 0.75f * std::clamp(eyes.openness[e], 0.f, 1.f) + 0.25f * std::clamp(eyes.widen[e], 0.f, 1.f); };
		send_float("EyeLidLeft", lid(0));
		send_float("EyeLidRight", lid(1));
		send_float("EyeLid", (lid(0) + lid(1)) / 2);
	}
	float dilation = 0;
	int n = 0;
	for (float d: eyes.dilation)
		if (d >= 0)
			dilation += d, n++;
	if (n)
		send_float("PupilDilation", dilation / n);
}
