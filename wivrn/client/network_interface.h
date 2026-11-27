// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <string>
#include <string_view>

namespace wivrn
{
enum class interface_type
{
	unknown,
	wifi,
	dongle,
	usb
};

inline interface_type classify_interface(std::string_view name, bool steam_vr, bool wireless)
{
	if (steam_vr and name == "wlanap")
		return interface_type::dongle;
	if (wireless or (steam_vr and name == "wlan0"))
		return interface_type::wifi;
	if (name.starts_with("usb"))
		return interface_type::usb;
	return interface_type::unknown;
}

interface_type interface_kind(std::string_view name);
std::string interface_display_name(std::string_view name);
} // namespace wivrn
