#pragma once

// SPDX-License-Identifier: GPL-3.0-or-later

#include "wivrn_packets.h"
#include <array>
#include <chrono>
#include <cstdlib>
#include <fcntl.h>
#include <string>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

namespace frame_shell
{
class focus_reader
{
	struct state
	{
		uint32_t version;
		uint32_t reserved;
		uint64_t heartbeat_ns;
		uint32_t flags;
	};
	const state * shared = nullptr;
	std::chrono::steady_clock::time_point retry_at{};
	std::array<bool, 256> held{};
	std::array<float, 256> last_value{};
	std::array<XrTime, 256> last_change{};

	uint32_t flags()
	{
		if (not shared and std::chrono::steady_clock::now() >= retry_at)
		{
			retry_at = std::chrono::steady_clock::now() + std::chrono::seconds(1);
			const char * override_path = std::getenv("WAYVR_OPENXR_LAYER_CONTROL_PATH");
			const char * home = std::getenv("HOME");
			std::string path = override_path ? override_path : std::string(home ? home : "/tmp") + "/.local/share/wayvr/openxr-layer-control";
			int fd = open(path.c_str(), O_RDONLY | O_CLOEXEC);
			if (fd >= 0)
			{
				struct stat info{};
				if (fstat(fd, &info) == 0 and info.st_size >= static_cast<off_t>(sizeof(state)))
				{
					void * map = mmap(nullptr, sizeof(state), PROT_READ, MAP_SHARED, fd, 0);
					if (map != MAP_FAILED)
						shared = static_cast<const state *>(map);
				}
				close(fd);
			}
		}
		if (not shared or __atomic_load_n(&shared->version, __ATOMIC_ACQUIRE) != 3)
			return 0;
		uint64_t heartbeat = __atomic_load_n(&shared->heartbeat_ns, __ATOMIC_ACQUIRE);
		uint64_t now = std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::system_clock::now().time_since_epoch()).count();
		if (now > heartbeat and now - heartbeat > 1'000'000'000)
			return 0;
		return __atomic_load_n(&shared->flags, __ATOMIC_ACQUIRE);
	}

	static bool blocked(wivrn::device_id id, uint32_t flags)
	{
		using enum wivrn::device_id;
		bool left = id >= LEFT_X_CLICK and id <= DPAD_LEFT_TOUCH;
		bool right = id >= A_CLICK and id <= RIGHT_GRASP_READY;
		if (not left and not right)
			return false;
		// Preserve passive sensing and the user's always-available mute bindings.
		switch (id)
		{
			case LEFT_X_TOUCH:
			case LEFT_Y_TOUCH:
			case VIEW_TOUCH:
			case LEFT_SYSTEM_TOUCH:
			case LEFT_BUMPER_TOUCH:
			case LEFT_SQUEEZE_TOUCH:
			case LEFT_TRIGGER_TOUCH:
			case LEFT_TRIGGER_PROXIMITY:
			case LEFT_TRIGGER_CURL:
			case LEFT_TRIGGER_SLIDE:
			case LEFT_THUMBSTICK_CLICK:
			case LEFT_THUMBSTICK_TOUCH:
			case LEFT_THUMBREST_TOUCH:
			case LEFT_THUMB_PROXIMITY:
			case LEFT_TRACKPAD_TOUCH:
			case DPAD_UP_TOUCH:
			case DPAD_LEFT_CLICK:
			case DPAD_LEFT_TOUCH:
			case A_TOUCH:
			case B_CLICK:
			case B_TOUCH:
			case RIGHT_X_TOUCH:
			case RIGHT_Y_TOUCH:
			case RIGHT_SYSTEM_TOUCH:
			case RIGHT_MENU_TOUCH:
			case RIGHT_BUMPER_TOUCH:
			case RIGHT_SQUEEZE_TOUCH:
			case RIGHT_TRIGGER_TOUCH:
			case RIGHT_TRIGGER_PROXIMITY:
			case RIGHT_TRIGGER_CURL:
			case RIGHT_TRIGGER_SLIDE:
			case RIGHT_THUMBSTICK_CLICK:
			case RIGHT_THUMBSTICK_TOUCH:
			case RIGHT_THUMBREST_TOUCH:
			case RIGHT_THUMB_PROXIMITY:
			case RIGHT_TRACKPAD_TOUCH:
				return false;
			default: break;
		}
		uint32_t mode = (flags >> (right ? 2 : 0)) & 3;
		bool trigger = (id >= LEFT_TRIGGER_CLICK and id <= LEFT_TRIGGER_FORCE)
		            or (id >= RIGHT_TRIGGER_CLICK and id <= RIGHT_TRIGGER_FORCE);
		return mode == 2 or (mode == 1 and trigger);
	}

public:
	focus_reader() = default;
	focus_reader(const focus_reader &) = delete;
	focus_reader & operator=(const focus_reader &) = delete;
	~focus_reader()
	{
		if (shared)
			munmap(const_cast<state *>(shared), sizeof(state));
	}

	void filter(wivrn::from_headset::inputs & inputs, XrTime now)
	{
		uint32_t current = flags();
		for (auto & input: inputs.values)
		{
			auto index = static_cast<uint8_t>(input.id);
			if (input.value == 0)
				held[index] = false;
			if (blocked(input.id, current) and input.value != 0)
				held[index] = true;
			bool suppress = blocked(input.id, current) or held[index];
			float value = suppress ? 0 : input.value;
			if (value != last_value[index])
				last_change[index] = now;
			input.value = last_value[index] = value;
			input.last_change_time = last_change[index];
		}
	}
};
}
