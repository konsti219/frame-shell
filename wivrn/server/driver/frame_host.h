// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "frame_abi.h"
#include "util/u_logging.h"
#include <cstdint>
#include <functional>
#include <span>
#include <stdexcept>
#include <string_view>

namespace wivrn
{
// The frame shell's PC host services (desktop/, linked in) for one headset connection: monitor video and
// surfaces, and opaque messages for the shell. Callbacks run on the host's threads until destruction.
class frame_host_session
{
public:
	struct callbacks
	{
		std::function<void(const frame_host_surface &)> surface;
		std::function<void(uint32_t)> surface_removed;
		std::function<void(uint32_t, std::span<const uint8_t>)> video;
		std::function<void(std::span<const uint8_t>)> message;
	};

private:
	callbacks handlers;
	frame_host * host = nullptr;

	// Exceptions must not unwind into the host's Rust code: network errors surface on the session's own threads
	template <typename F>
	static void guarded(F && f) noexcept
	{
		try
		{
			f();
		}
		catch (const std::exception & e)
		{
			U_LOG_W("Frame host callback: %s", e.what());
		}
		catch (...)
		{
		}
	}

public:
	explicit frame_host_session(callbacks handlers_) :
	        handlers(std::move(handlers_))
	{
		frame_host_callbacks c{
		        .user = this,
		        .surface = [](void * user, const frame_host_surface * surface) {
			        guarded([&] { static_cast<frame_host_session *>(user)->handlers.surface(*surface); });
		        },
		        .surface_removed = [](void * user, uint32_t id) {
			        guarded([&] { static_cast<frame_host_session *>(user)->handlers.surface_removed(id); });
		        },
		        .video = [](void * user, uint32_t surface, const uint8_t * data, size_t size) {
			        guarded([&] { static_cast<frame_host_session *>(user)->handlers.video(surface, {data, size}); });
		        },
		        .message = [](void * user, const uint8_t * data, size_t size) {
			        guarded([&] { static_cast<frame_host_session *>(user)->handlers.message({data, size}); });
		        },
		};
		host = frame_host_start(&c);
		if (!host)
			throw std::runtime_error("Frame shell host services failed to start");
	}
	frame_host_session(const frame_host_session &) = delete;
	frame_host_session & operator=(const frame_host_session &) = delete;
	~frame_host_session()
	{
		frame_host_stop(host);
	}

	void receive(std::span<const uint8_t> message)
	{
		frame_host_receive(host, message.data(), message.size());
	}
	void request_keyframe(uint32_t surface)
	{
		frame_host_request_keyframe(host, surface);
	}
};
} // namespace wivrn
