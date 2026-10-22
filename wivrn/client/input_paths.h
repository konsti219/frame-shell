#pragma once
#include "wivrn_packets.h"
#include <string>
#include <unordered_map>
#include <array>
#include <numeric>
namespace wivrn {
// clang-format off
inline const std::unordered_map<std::string, device_id> device_ids = {
	{"/user/hand/left/input/dpad_up/click",       device_id::DPAD_UP_CLICK},
	{"/user/hand/left/input/dpad_up/touch",       device_id::DPAD_UP_TOUCH},
	{"/user/hand/left/input/dpad_down/click",     device_id::LEFT_X_CLICK},
	{"/user/hand/left/input/dpad_down/touch",     device_id::LEFT_X_TOUCH},
	{"/user/hand/left/input/dpad_left/click",     device_id::DPAD_LEFT_CLICK},
	{"/user/hand/left/input/dpad_left/touch",     device_id::DPAD_LEFT_TOUCH},
	{"/user/hand/left/input/dpad_right/click",    device_id::LEFT_Y_CLICK},
	{"/user/hand/left/input/dpad_right/touch",    device_id::LEFT_Y_TOUCH},
	{"/user/hand/left/input/x/click",             device_id::LEFT_X_CLICK},
	{"/user/hand/left/input/x/touch",             device_id::LEFT_X_TOUCH},
	{"/user/hand/left/input/y/click",             device_id::LEFT_Y_CLICK},
	{"/user/hand/left/input/y/touch",             device_id::LEFT_Y_TOUCH},
	{"/user/hand/left/input/view/click",          device_id::VIEW_CLICK},
	{"/user/hand/left/input/view/touch",          device_id::VIEW_TOUCH},
	{"/user/hand/left/input/menu/click",          device_id::LEFT_MENU_CLICK},
	{"/user/hand/left/input/system/click",        device_id::LEFT_MENU_CLICK},
	{"/user/hand/left/input/system/touch",        device_id::LEFT_SYSTEM_TOUCH},
	{"/user/hand/left/input/bumper/click",        device_id::LEFT_BUMPER_CLICK},
	{"/user/hand/left/input/bumper/touch",        device_id::LEFT_BUMPER_TOUCH},
	{"/user/hand/left/input/squeeze/click",       device_id::LEFT_SQUEEZE_CLICK},
	{"/user/hand/left/input/squeeze/force",       device_id::LEFT_SQUEEZE_FORCE},
	{"/user/hand/left/input/squeeze/touch",       device_id::LEFT_SQUEEZE_TOUCH},
	{"/user/hand/left/input/squeeze/value",       device_id::LEFT_SQUEEZE_VALUE},
	{"/user/hand/left/input/trigger/value",       device_id::LEFT_TRIGGER_VALUE},
	{"/user/hand/left/input/trigger/click",       device_id::LEFT_TRIGGER_CLICK},
	{"/user/hand/left/input/trigger/touch",       device_id::LEFT_TRIGGER_TOUCH},
	{"/user/hand/left/input/trigger/proximity",   device_id::LEFT_TRIGGER_PROXIMITY},
	{"/user/hand/left/input/trigger/proximity_fb",device_id::LEFT_TRIGGER_PROXIMITY},
	{"/user/hand/left/input/trigger/proximity_meta",device_id::LEFT_TRIGGER_PROXIMITY},
	{"/user/hand/left/input/trigger/curl_fb",     device_id::LEFT_TRIGGER_CURL},
	{"/user/hand/left/input/trigger/curl_meta",   device_id::LEFT_TRIGGER_CURL},
	{"/user/hand/left/input/trigger_curl/value",  device_id::LEFT_TRIGGER_CURL},
	{"/user/hand/left/input/trigger/slide_fb",    device_id::LEFT_TRIGGER_SLIDE},
	{"/user/hand/left/input/trigger/slide_meta",  device_id::LEFT_TRIGGER_SLIDE},
	{"/user/hand/left/input/trigger_slide/value", device_id::LEFT_TRIGGER_SLIDE},
	{"/user/hand/left/input/trigger/force",       device_id::LEFT_TRIGGER_FORCE},
	{"/user/hand/left/input/thumbstick",          device_id::LEFT_THUMBSTICK_X},
	{"/user/hand/left/input/thumbstick/click",    device_id::LEFT_THUMBSTICK_CLICK},
	{"/user/hand/left/input/thumbstick/touch",    device_id::LEFT_THUMBSTICK_TOUCH},
	{"/user/hand/left/input/thumbrest/touch",     device_id::LEFT_THUMBREST_TOUCH},
	{"/user/hand/left/input/thumbrest/force",     device_id::LEFT_THUMBREST_FORCE},
	{"/user/hand/left/input/thumb_resting_surfaces/proximity",device_id::LEFT_THUMB_PROXIMITY},
	{"/user/hand/left/input/thumb_meta/proximity_meta",device_id::LEFT_THUMB_PROXIMITY},
	{"/user/hand/left/input/trackpad",            device_id::LEFT_TRACKPAD_X},
	{"/user/hand/left/input/trackpad/click",      device_id::LEFT_TRACKPAD_CLICK},
	{"/user/hand/left/input/trackpad/touch",      device_id::LEFT_TRACKPAD_TOUCH},
	{"/user/hand/left/input/trackpad/force",      device_id::LEFT_TRACKPAD_FORCE},
	{"/user/hand/left/input/stylus/force",        device_id::LEFT_STYLUS_FORCE},
	{"/user/hand/left/input/stylus_fb/force",     device_id::LEFT_STYLUS_FORCE},

	{"/user/hand/right/input/a/click",             device_id::A_CLICK},
	{"/user/hand/right/input/a/touch",             device_id::A_TOUCH},
	{"/user/hand/right/input/b/click",             device_id::B_CLICK},
	{"/user/hand/right/input/b/touch",             device_id::B_TOUCH},
	{"/user/hand/right/input/x/click",             device_id::RIGHT_X_CLICK},
	{"/user/hand/right/input/x/touch",             device_id::RIGHT_X_TOUCH},
	{"/user/hand/right/input/y/click",             device_id::RIGHT_Y_CLICK},
	{"/user/hand/right/input/y/touch",             device_id::RIGHT_Y_TOUCH},
	{"/user/hand/right/input/system/click",        device_id::RIGHT_SYSTEM_CLICK},
	{"/user/hand/right/input/system/touch",        device_id::RIGHT_SYSTEM_TOUCH},
	{"/user/hand/right/input/menu/click",          device_id::RIGHT_MENU_CLICK},
	{"/user/hand/right/input/menu/touch",          device_id::RIGHT_MENU_TOUCH},
	{"/user/hand/right/input/bumper/click",        device_id::RIGHT_BUMPER_CLICK},
	{"/user/hand/right/input/bumper/touch",        device_id::RIGHT_BUMPER_TOUCH},
	{"/user/hand/right/input/squeeze/click",       device_id::RIGHT_SQUEEZE_CLICK},
	{"/user/hand/right/input/squeeze/force",       device_id::RIGHT_SQUEEZE_FORCE},
	{"/user/hand/right/input/squeeze/touch",       device_id::RIGHT_SQUEEZE_TOUCH},
	{"/user/hand/right/input/squeeze/value",       device_id::RIGHT_SQUEEZE_VALUE},
	{"/user/hand/right/input/trigger/value",       device_id::RIGHT_TRIGGER_VALUE},
	{"/user/hand/right/input/trigger/click",       device_id::RIGHT_TRIGGER_CLICK},
	{"/user/hand/right/input/trigger/touch",       device_id::RIGHT_TRIGGER_TOUCH},
	{"/user/hand/right/input/trigger/proximity",   device_id::RIGHT_TRIGGER_PROXIMITY},
	{"/user/hand/right/input/trigger/proximity_fb",device_id::RIGHT_TRIGGER_PROXIMITY},
	{"/user/hand/right/input/trigger/proximity_meta",device_id::RIGHT_TRIGGER_PROXIMITY},
	{"/user/hand/right/input/trigger/curl_fb",     device_id::RIGHT_TRIGGER_CURL},
	{"/user/hand/right/input/trigger/curl_meta",   device_id::RIGHT_TRIGGER_CURL},
	{"/user/hand/right/input/trigger_curl/value",  device_id::RIGHT_TRIGGER_CURL},
	{"/user/hand/right/input/trigger/slide_fb",    device_id::RIGHT_TRIGGER_SLIDE},
	{"/user/hand/right/input/trigger/slide_meta",  device_id::RIGHT_TRIGGER_SLIDE},
	{"/user/hand/right/input/trigger_slide/value", device_id::RIGHT_TRIGGER_SLIDE},
	{"/user/hand/right/input/trigger/force",       device_id::RIGHT_TRIGGER_FORCE},
	{"/user/hand/right/input/thumbstick",          device_id::RIGHT_THUMBSTICK_X},
	{"/user/hand/right/input/thumbstick/click",    device_id::RIGHT_THUMBSTICK_CLICK},
	{"/user/hand/right/input/thumbstick/touch",    device_id::RIGHT_THUMBSTICK_TOUCH},
	{"/user/hand/right/input/thumbrest/touch",     device_id::RIGHT_THUMBREST_TOUCH},
	{"/user/hand/right/input/thumbrest/force",     device_id::RIGHT_THUMBREST_FORCE},
	{"/user/hand/right/input/thumb_resting_surfaces/proximity",device_id::RIGHT_THUMB_PROXIMITY},
	{"/user/hand/right/input/thumb_meta/proximity_meta",device_id::RIGHT_THUMB_PROXIMITY},
	{"/user/hand/right/input/trackpad",            device_id::RIGHT_TRACKPAD_X},
	{"/user/hand/right/input/trackpad/click",      device_id::RIGHT_TRACKPAD_CLICK},
	{"/user/hand/right/input/trackpad/touch",      device_id::RIGHT_TRACKPAD_TOUCH},
	{"/user/hand/right/input/trackpad/force",      device_id::RIGHT_TRACKPAD_FORCE},
	{"/user/hand/right/input/stylus/force",        device_id::RIGHT_STYLUS_FORCE},
	{"/user/hand/right/input/stylus_fb/force",     device_id::RIGHT_STYLUS_FORCE},

	// XR_EXT_hand_interaction
	{"/user/hand/left/input/pinch_ext/value",      device_id::LEFT_PINCH_VALUE},
	{"/user/hand/left/input/pinch_ext/ready_ext",  device_id::LEFT_PINCH_READY},
	{"/user/hand/left/input/aim_activate_ext/value",device_id::LEFT_AIM_ACTIVATE_VALUE},
	{"/user/hand/left/input/aim_activate_ext/ready_ext",device_id::LEFT_AIM_ACTIVATE_READY},
	{"/user/hand/left/input/grasp_ext/value",      device_id::LEFT_GRASP_VALUE},
	{"/user/hand/left/input/grasp_ext/ready_ext",  device_id::LEFT_GRASP_READY},

	{"/user/hand/right/input/pinch_ext/value",      device_id::RIGHT_PINCH_VALUE},
	{"/user/hand/right/input/pinch_ext/ready_ext",  device_id::RIGHT_PINCH_READY},
	{"/user/hand/right/input/aim_activate_ext/value",device_id::RIGHT_AIM_ACTIVATE_VALUE},
	{"/user/hand/right/input/aim_activate_ext/ready_ext",device_id::RIGHT_AIM_ACTIVATE_READY},
	{"/user/hand/right/input/grasp_ext/value",      device_id::RIGHT_GRASP_VALUE},
	{"/user/hand/right/input/grasp_ext/ready_ext",  device_id::RIGHT_GRASP_READY},

	{"/user/gamepad/input/menu/click",             device_id::GAMEPAD_MENU_CLICK},
	{"/user/gamepad/input/view/click",             device_id::GAMEPAD_VIEW_CLICK},
	{"/user/gamepad/input/a/click",                device_id::GAMEPAD_A_CLICK},
	{"/user/gamepad/input/b/click",                device_id::GAMEPAD_B_CLICK},
	{"/user/gamepad/input/x/click",                device_id::GAMEPAD_X_CLICK},
	{"/user/gamepad/input/y/click",                device_id::GAMEPAD_Y_CLICK},
	{"/user/gamepad/input/dpad_down/click",        device_id::GAMEPAD_DPAD_DOWN_CLICK},
	{"/user/gamepad/input/dpad_right/click",       device_id::GAMEPAD_DPAD_RIGHT_CLICK},
	{"/user/gamepad/input/dpad_up/click",          device_id::GAMEPAD_DPAD_UP_CLICK},
	{"/user/gamepad/input/dpad_left/click",        device_id::GAMEPAD_DPAD_LEFT_CLICK},
	{"/user/gamepad/input/shoulder_left/click",    device_id::GAMEPAD_SHOULDER_LEFT_CLICK},
	{"/user/gamepad/input/shoulder_right/click",   device_id::GAMEPAD_SHOULDER_RIGHT_CLICK},
	{"/user/gamepad/input/thumbstick_left/click",  device_id::GAMEPAD_THUMBSTICK_LEFT_CLICK},
	{"/user/gamepad/input/thumbstick_right/click", device_id::GAMEPAD_THUMBSTICK_RIGHT_CLICK},
	{"/user/gamepad/input/trigger_left/value",     device_id::GAMEPAD_TRIGGER_LEFT_VALUE},
	{"/user/gamepad/input/trigger_right/value",    device_id::GAMEPAD_TRIGGER_RIGHT_VALUE},
	{"/user/gamepad/input/thumbstick_left/x",      device_id::GAMEPAD_THUMBSTICK_LEFT_X},
	{"/user/gamepad/input/thumbstick_left/y",      device_id::GAMEPAD_THUMBSTICK_LEFT_Y},
	{"/user/gamepad/input/thumbstick_right/x",     device_id::GAMEPAD_THUMBSTICK_RIGHT_X},
	{"/user/gamepad/input/thumbstick_right/y",     device_id::GAMEPAD_THUMBSTICK_RIGHT_Y},
};
// clang-format on
inline const auto input_groups = [] {
    std::array<uint8_t, 256> groups;
    std::iota(groups.begin(), groups.end(), uint8_t{0});
    auto join = [&](uint8_t a, uint8_t b) {
        uint8_t from = groups[b], to = groups[a];
        for (auto & group : groups) if (group == from) group = to;
    };
    for (const auto & [path, device] : device_ids) {
        auto id = static_cast<uint8_t>(device);
        auto split = path.rfind('/');
        auto component = path.substr(split + 1);
        auto base = path.substr(0, split);
        if (component == "click" || component == "value" || component == "force") {
            for (auto alias : {"click", "value", "force"}) {
                if (auto it = device_ids.find(base + "/" + alias); it != device_ids.end())
                    join(id, static_cast<uint8_t>(it->second));
            }
        }
        if (path.ends_with("/thumbstick") || path.ends_with("/trackpad")) join(id, id + 1);
        if (component == "x") {
            if (auto it = device_ids.find(base + "/y"); it != device_ids.end())
                join(id, static_cast<uint8_t>(it->second));
        }
    }
    return groups;
}();

}
