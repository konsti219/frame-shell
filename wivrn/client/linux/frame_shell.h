// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "configuration.h"
#include "decoder/decoder.h"
#include "frame_abi.h"
#include "input_paths.h"
#include <algorithm>
#include <array>
#include <atomic>
#include <charconv>
#include <chrono>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <deque>
#include <dlfcn.h>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <span>
#include <spdlog/fmt/fmt.h>
#include <spdlog/spdlog.h>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <utility>
#include <vector>

namespace frame_shell
{
struct input_capture_state {
    std::array<bool, 256> inputs{};
    uint32_t pose_mask = 0;
};
inline std::mutex input_capture_mutex;
inline input_capture_state input_capture;
inline input_capture_state input_capture_snapshot() {
    std::scoped_lock lock(input_capture_mutex);
    return input_capture;
}
inline void set_input_capture(const char * const * paths, size_t count, uint32_t pose_mask) {
    input_capture_state next{.pose_mask = pose_mask};
    for (size_t i = 0; i < count; ++i) {
        if (auto it = wivrn::device_ids.find(paths[i]); it != wivrn::device_ids.end()) {
            auto id = static_cast<uint8_t>(it->second);
            next.inputs[id] = true;
            std::string_view path(paths[i]);
            if (path.ends_with("/thumbstick") or path.ends_with("/trackpad"))
                next.inputs[id + 1] = true;
        }
    }
    std::scoped_lock lock(input_capture_mutex);
    input_capture = next;
}

// WayVR's entry points (frame_abi.h), null without FRAME_SHELL_LIBRARY
inline const frame_shell_api * api = nullptr;
inline bool enabled() { return api != nullptr; }

using input = frame_shell_input;
using desktop_surface_info = frame_shell_desktop_surface;
using desktop_frame_info = frame_shell_desktop_frame;

inline bool input_poll(input * event) { return api->input_poll(event); }
inline void desktop_surface(const desktop_surface_info * info) { api->desktop_surface(info); }
inline void connected(uint32_t source, bool value, bool vr) { api->connected(source, value, vr); }
inline void lobby_state(const char * json) { api->lobby_state(json); }
inline void stream_state(uint32_t source, const char * json)
{
	api->stream_state(source, json);
}
inline void host_message(uint32_t source, std::span<const uint8_t> data) { api->host_message(source, data.data(), data.size()); }
inline void hosts_state(const frame_shell_pc * pcs, size_t count, uint32_t selected)
{
	api->hosts_state(pcs, count, selected);
}

inline std::atomic<bool> switch_wanted{false};

struct runtime_activity
{
	bool initialized = false, known = false, visible = true, overlay_visible = false, composition_supported = false;
	std::set<uint32_t> defaults;
	int64_t since = 0;
};
inline std::mutex activity_mutex;
inline std::map<uint32_t, runtime_activity> activities;
inline runtime_activity activity(uint32_t source)
{
	std::scoped_lock lock(activity_mutex);
	auto it = activities.find(source);
	return it == activities.end() ? runtime_activity{} : it->second;
}
inline void runtime_clients(uint32_t source, const frame_runtime_client * clients, size_t count, bool known, bool composition_supported)
{
	std::scoped_lock lock(activity_mutex);
	auto & activity = activities[source];
	activity.known = known;
	activity.composition_supported = composition_supported;
	if (!known)
		return;
	activity.visible = std::any_of(clients, clients + count, [](const auto & client) { return client.visible; });
	const bool overlay_visible = std::any_of(clients, clients + count, [](const auto & client) { return client.overlay && client.visible; });
	if (overlay_visible != activity.overlay_visible)
		switch_wanted = true;
	activity.overlay_visible = overlay_visible;
	std::set<uint32_t> defaults;
	for (size_t i = 0; i < count; ++i)
		if (!clients[i].overlay)
			defaults.insert(clients[i].id);
	bool added = std::ranges::any_of(defaults, [&](auto id) { return !activity.defaults.contains(id); });
	if (defaults.empty())
		activity.since = 0;
	else if (!activity.initialized)
		activity.since = 1;
	else if (added)
		activity.since = std::chrono::steady_clock::now().time_since_epoch().count();
	if (activity.defaults != defaults)
		switch_wanted = true;
	activity.initialized = true;
	activity.defaults = std::move(defaults);
}

// A PC on standby started a VR application: the presenting stream gives way so the lobby can switch

// The shell asked for a PC (cookie) to present VR ("present <cookie>"), handled by the lobby
inline std::mutex present_mutex;
inline std::string present_request;

inline void request_present(std::string cookie)
{
    std::scoped_lock lock(present_mutex);
    present_request = std::move(cookie);
    switch_wanted = true;
}

inline std::string take_present_request()
{
    std::scoped_lock lock(present_mutex);
    return std::exchange(present_request, {});
}
inline bool command_poll(char * buffer, size_t size) { return api->command_poll(buffer, size); }
inline void drag_offset(float * out) { api->drag_offset(out); }

// Messages from the shell for each connected PC (source), sent by that connection's network thread
inline std::mutex host_messages_mutex;
inline std::map<uint32_t, std::deque<std::vector<uint8_t>>> host_messages;

inline std::vector<std::vector<uint8_t>> take_host_messages(uint32_t source)
{
    std::scoped_lock lock(host_messages_mutex);
    auto it = host_messages.find(source);
    if (it == host_messages.end()) return {};
    std::vector<std::vector<uint8_t>> taken(std::make_move_iterator(it->second.begin()), std::make_move_iterator(it->second.end()));
    host_messages.erase(it);
    return taken;
}

// PC input from the shell, sorted by PC (source) by whichever connection polls first
inline std::mutex input_mutex;
inline std::map<uint32_t, std::deque<input>> inputs;

inline std::vector<input> take_input(uint32_t source)
{
    std::scoped_lock lock(input_mutex);
    input event;
    for (int count = 0; count < 256 && input_poll(&event); ++count) {
        auto & queue = inputs[event.source];
        if (queue.size() < 512) queue.push_back(event);
    }
    auto it = inputs.find(source);
    if (it == inputs.end()) return {};
    std::vector<input> taken(it->second.begin(), it->second.end());
    inputs.erase(it);
    return taken;
}

// A PC went away: what was queued for it is moot
inline void forget_source(uint32_t source)
{
	{
		std::scoped_lock lock(activity_mutex);
		activities.erase(source);
	}
    {
        std::scoped_lock lock(host_messages_mutex);
        host_messages.erase(source);
    }
    std::scoped_lock lock(input_mutex);
    inputs.erase(source);
}

// Exported buffers outlive their decoder until the shell releases them after GPU use.
inline std::mutex held_mutex;
inline std::map<uint64_t, std::shared_ptr<void>> held_frames;

inline void present_desktop(std::shared_ptr<void> handle, const desktop_frame_info & info)
{
    {
        std::scoped_lock lock(held_mutex);
        held_frames.emplace(info.id, std::move(handle));
    }
    api->desktop_frame(&info);
}

// Tints the defoveated PC stream by its resolution (dashboard debug view, not saved)
inline std::atomic<bool> foveation_debug{false};

inline std::string settings_json(const configuration & config)
{
    const char * codec = "auto";
    if (config.codec) {
        switch (*config.codec) {
        case wivrn::video_codec::h264: codec = "h264"; break;
        case wivrn::video_codec::h265: codec = "h265"; break;
        case wivrn::video_codec::av1: codec = "av1"; break;
        case wivrn::video_codec::raw: codec = "raw"; break;
        }
    }
    return fmt::format(R"({{"bitrate":{},"max_bitrate":{},"fec":{},"frame_margin":{},"retransmit":{},"resolution":{},"foveation":{},"max_foveation":{},"codec":"{}","foveation_debug":{},"desktop_bitrate":{},"desktop_codec":"{}","codec_support":{{"h264":{},"h265":{},"av1":{}}}}})",
                       config.bitrate_bps, config.max_bitrate(),
                       config.fec_percent, config.frame_margin_ms, config.retransmit, config.resolution_scale,
                       config.chosen_stream_scale(), 100, codec,
                       foveation_debug.load(), config.desktop_bitrate_kbps,
                       config.desktop_codec == wivrn::video_codec::h265 ? "h265" : "h264",
                       wivrn::decoder::supports_codec(wivrn::video_codec::h264),
                       wivrn::decoder::supports_codec(wivrn::video_codec::h265),
                       wivrn::decoder::supports_codec(wivrn::video_codec::av1));
}

inline bool apply_setting(configuration & config, std::string_view command)
{
    if (!command.starts_with("setting ")) return false;
    auto separator = command.find(' ', 8);
    if (separator == std::string_view::npos) return false;
    auto key = command.substr(8, separator - 8);
    auto value = command.substr(separator + 1);
    auto parse = [value](auto & number) {
        auto [end, error] = std::from_chars(value.data(), value.data() + value.size(), number);
        return error == std::errc{} && end == value.data() + value.size();
    };
    if (key == "retransmit") {
        config.retransmit = value == "1";
        config.save();
        return true;
    }
    if (key == "foveation_debug") {
        foveation_debug = value == "1";
        return true;
    }
    if (key == "bitrate") {
        uint32_t bitrate;
        if (!parse(bitrate) || bitrate < 1'000'000 || bitrate > config.max_bitrate()) return false;
        config.bitrate_bps = bitrate;
    } else if (key == "frame_margin") {
        uint32_t ms;
        if (!parse(ms) || ms > 20) return false;
        config.frame_margin_ms = ms;
    } else if (key == "fec") {
        // Percent parity
        uint32_t percent;
        if (!parse(percent) || percent > 50) return false;
        config.fec_percent = percent;
    } else if (key == "resolution") {
        float scale;
        if (!parse(scale) || !std::isfinite(scale) || scale < 0.5f || scale > 2.f) return false;
        config.resolution_scale = scale;
    } else if (key == "foveation") {
        // Streamed size as a fraction of the render size; 1 streams it whole (the decoder's size limit applies when used)
        float scale;
        if (!parse(scale) || !std::isfinite(scale) || scale < 0.2f || scale > 1.f) return false;
        config.set_stream_scale(scale);
    } else if (key == "desktop_bitrate") {
        // kbit/s per 1080p
        uint32_t bitrate;
        if (!parse(bitrate) || bitrate < 1'000 || bitrate > 150'000) return false;
        config.desktop_bitrate_kbps = bitrate;
    } else if (key == "desktop_codec") {
        wivrn::video_codec requested;
        if (value == "h264") requested = wivrn::video_codec::h264;
        else if (value == "h265") requested = wivrn::video_codec::h265;
        else return false;
        if (!wivrn::decoder::supports_codec(requested)) return false;
        config.desktop_codec = requested;
    } else if (key == "codec") {
        std::optional<wivrn::video_codec> requested;
        if (value == "auto") requested.reset();
        else if (value == "h264") requested = wivrn::video_codec::h264;
        else if (value == "h265") requested = wivrn::video_codec::h265;
        else if (value == "av1") requested = wivrn::video_codec::av1;
        else return false;
        if (requested && !wivrn::decoder::supports_codec(*requested)) return false;
        config.codec = requested;
    } else return false;
    config.save();
    return true;
}

// Set when the user disconnected from the shell's UI, so the lobby doesn't reconnect on its own
inline std::atomic<bool> user_disconnected{false};

// Next queued shell command that `accept` takes; the others stay queued for the scene that handles them.
inline std::mutex command_mutex;
inline std::deque<std::string> command_queue;

// Commands for a PC that are moot once disconnected
inline void clear_stream_commands(uint32_t source, bool background)
{
    std::scoped_lock lock(command_mutex);
    const auto prefix = fmt::format("on {} ", source);
    std::erase_if(command_queue, [&](const auto & command) {
	    return command.starts_with(prefix) || (background && (command.starts_with("stop_app ") || command.starts_with("set_active_app ")));
    });
}

template <typename Accept>
std::optional<std::string> take_command(Accept && accept)
{
    std::scoped_lock lock(command_mutex);
    auto & queue = command_queue;
    char buffer[512];
    while (command_poll(buffer, sizeof(buffer))) {
	    std::string_view command(buffer);
	    if (command.starts_with("connect ") || command.starts_with("disconnect") ||
		command.starts_with("pin_vr ") || command == "auto_vr" || command.starts_with("pin ") ||
		command == "pin_cancel" || command.starts_with("autoconnect ") || command.starts_with("forget "))
		    switch_wanted = true;
	    queue.emplace_back(buffer);
	    if (queue.size() > 32)
		    queue.pop_front();
    }
    for (auto it = queue.begin(); it != queue.end(); ++it) {
        if (accept(*it)) {
            std::string command = std::move(*it);
            queue.erase(it);
            return command;
        }
    }
    return std::nullopt;
}

class runtime
{
    std::jthread thread;
    void * library = nullptr;
    frame_shell_host host{
	    .user = nullptr,
	    .release_desktop_frame = [](void *, uint64_t id) {
                std::shared_ptr<void> handle;
                std::scoped_lock lock(held_mutex);
                if (auto it = held_frames.find(id); it != held_frames.end())
                {
                    // Dropped after the lock: giving the buffer back may wake the decoder
                    handle = std::move(it->second);
                    held_frames.erase(it);
                } },
	    .send_host_message = [](void *, uint32_t source, const uint8_t * data, size_t size) {
                std::scoped_lock lock(host_messages_mutex);
                auto & queue = host_messages[source];
                if (queue.size() < 256)
                    queue.emplace_back(data, data + size); },
	    .set_input_capture = [](void *, const char * const * paths, size_t count, uint32_t pose_mask) { set_input_capture(paths, count, pose_mask); },
	    .runtime_clients = [](void *, uint32_t source, const frame_runtime_client * clients, size_t count, bool known, bool composition_supported) { runtime_clients(source, clients, count, known, composition_supported); },
    };

    public:
    runtime()
    {
        const char * path = std::getenv("FRAME_SHELL_LIBRARY");
        if (!path) return;
        library = dlopen(path, RTLD_NOW | RTLD_LOCAL);
        if (!library) throw std::runtime_error(dlerror());
        auto entry = reinterpret_cast<frame_shell_api_fn>(dlsym(library, "frame_shell_api"));
        if (!entry) throw std::runtime_error(dlerror());
        const frame_shell_api * table = entry();
        if (!table || table->version != FRAME_SHELL_ABI_VERSION)
            throw std::runtime_error("Frame shell library has another ABI version");
        api = table;
        thread = std::jthread([run = api->run, host = &host] {
            int result = run(host);
            set_input_capture(nullptr, 0, 0);
            if (result != 0) {
                std::fprintf(stderr, "Frame shell failed (%d)\n", result);
                std::_Exit(EXIT_FAILURE);
            }
        });
    }
    ~runtime()
    {
        if (enabled()) {
            api->stop();
            thread.join();
            // Keep Rust/GStreamer thread-local destructors valid until process exit.
        }
    }
};
}
