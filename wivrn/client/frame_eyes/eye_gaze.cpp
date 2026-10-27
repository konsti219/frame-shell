/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "eye_gaze.h"

#include "babble/core/clock.h"

#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <spdlog/spdlog.h>

#include <fcntl.h>
#include <pthread.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

namespace
{
constexpr const char * path = "/dev/shm/eye-server.mmap";
constexpr uint64_t stale_ns = 200'000'000;

constexpr size_t off_version = 0x00;
constexpr size_t off_initialized = 0x04;
constexpr size_t off_mutex = 0x08;
constexpr size_t off_sequence = 0x38;
constexpr size_t off_requested = 0x3c;

constexpr size_t rec_producer_state = 0x00;
constexpr size_t rec_gaze = 0x0d; // float[2][3], left and right eye after fusion

bool layout(uint32_t version, size_t * size, size_t * record)
{
	switch (version)
	{
		case 4:
			*size = 0x4f21a, *record = 0x152;
			return true;
		case 5:
			*size = 0x4f21f, *record = 0x157;
			return true;
		default:
			return false;
	}
}

template <typename T>
T read_at(const uint8_t * base, size_t offset)
{
	T value;
	std::memcpy(&value, base + offset, sizeof(T));
	return value;
}

// corrected = (measured - offset) / gain per axis, in degrees (yaw positive right, pitch positive up)
struct correction
{
	double yaw_gain = 1, yaw_offset = 0, pitch_gain = 1, pitch_offset = 0;
};

correction read_correction()
{
	static correction cached;
	static uint64_t next_read_ns;
	uint64_t now = babble::mono_ns();
	if (now < next_read_ns)
		return cached;
	next_read_ns = now + 1'000'000'000;
	char file_path[512];
	const char * config = getenv("XDG_CONFIG_HOME");
	const char * home = getenv("HOME");
	if (config and *config)
		snprintf(file_path, sizeof(file_path), "%s/monado/eye-calibration", config);
	else if (home)
		snprintf(file_path, sizeof(file_path), "%s/.config/monado/eye-calibration", home);
	else
		return cached;
	FILE * file = fopen(file_path, "r");
	if (not file)
		return cached = {};
	correction c;
	char line[256];
	while (fgets(line, sizeof(line), file))
	{
		char key[32];
		double value;
		if (sscanf(line, "%31s %lf", key, &value) != 2)
			continue;
		if (strcmp(key, "yaw_gain") == 0 and value > 0.5 and value < 2)
			c.yaw_gain = value;
		else if (strcmp(key, "yaw_offset") == 0 and std::abs(value) < 20)
			c.yaw_offset = value;
		else if (strcmp(key, "pitch_gain") == 0 and value > 0.5 and value < 2)
			c.pitch_gain = value;
		else if (strcmp(key, "pitch_offset") == 0 and std::abs(value) < 20)
			c.pitch_offset = value;
	}
	fclose(file);
	return cached = c;
}

bool normalize_and_correct(XrVector3f & dir, const correction & c)
{
	float len = std::sqrt(dir.x * dir.x + dir.y * dir.y + dir.z * dir.z);
	if (not(len > 1e-6f) or not std::isfinite(len))
		return false;
	dir = {dir.x / len, dir.y / len, dir.z / len};
	constexpr double deg = M_PI / 180;
	double yaw = std::atan2(dir.x, -dir.z) / deg;
	double pitch = std::atan2(dir.y, std::hypot(dir.x, dir.z)) / deg;
	yaw = (yaw - c.yaw_offset) / c.yaw_gain * deg;
	pitch = (pitch - c.pitch_offset) / c.pitch_gain * deg;
	dir = {float(std::sin(yaw) * std::cos(pitch)), float(std::sin(pitch)), float(-std::cos(yaw) * std::cos(pitch))};
	return true;
}
} // namespace

frame_eyes::eye_gaze::~eye_gaze()
{
	if (base)
		munmap(base, map_size);
}

bool frame_eyes::eye_gaze::open(uint64_t now_ns)
{
	if (now_ns < next_open_ns)
		return false;
	// The file only exists while Valve's eye tracker runs
	next_open_ns = now_ns + 1'000'000'000;
	int fd = ::open(path, O_RDWR);
	if (fd < 0)
		return false;
	struct stat st;
	void * mem = MAP_FAILED;
	if (fstat(fd, &st) == 0 and st.st_size >= 0x40)
		mem = mmap(nullptr, st.st_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	close(fd);
	if (mem == MAP_FAILED)
		return false;
	size_t needed = 0;
	uint32_t version = read_at<uint32_t>((uint8_t *)mem, off_version);
	if (not layout(version, &needed, &record) or size_t(st.st_size) < needed)
	{
		spdlog::warn("Eyes: unsupported eye tracking shared memory version {}", version);
		munmap(mem, st.st_size);
		return false;
	}
	base = (uint8_t *)mem;
	map_size = st.st_size;
	spdlog::info("Eyes: reading per-eye gaze from {} (version {})", path, version);
	return true;
}

bool frame_eyes::eye_gaze::read(std::array<XrVector3f, 2> & out)
{
	uint64_t now = babble::mono_ns();
	if (not base and not open(now))
		return false;
	if (read_at<uint32_t>(base, off_initialized) == 1)
	{
		auto * mutex = reinterpret_cast<pthread_mutex_t *>(base + off_mutex);
		int ret = pthread_mutex_trylock(mutex);
		if (ret == EOWNERDEAD)
			pthread_mutex_consistent(mutex);
		if (ret == 0 or ret == EOWNERDEAD)
		{
			// Consumers ask for records; Valve's server bumps the sequence per record
			*reinterpret_cast<volatile uint32_t *>(base + off_requested) = 1;
			uint32_t sequence = *reinterpret_cast<volatile uint32_t *>(base + off_sequence);
			uint8_t rec[0x40];
			std::memcpy(rec, base + record, sizeof(rec));
			pthread_mutex_unlock(mutex);

			if (sequence != last_sequence and read_at<uint32_t>(rec, rec_producer_state) == 1)
			{
				last_sequence = sequence;
				const correction c = read_correction();
				std::array<XrVector3f, 2> gaze{read_at<XrVector3f>(rec, rec_gaze), read_at<XrVector3f>(rec, rec_gaze + 12)};
				if (normalize_and_correct(gaze[0], c) and normalize_and_correct(gaze[1], c))
				{
					latest = gaze;
					received_ns = now;
				}
			}
		}
	}
	if (received_ns == 0 or now - received_ns > stale_ns)
		return false;
	out = latest;
	return true;
}
