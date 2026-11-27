// SPDX-License-Identifier: GPL-3.0-or-later
#include "network_interface.h"
#include "utils/i18n.h"
#include <filesystem>
#include <fstream>
#include <map>

namespace wivrn
{
interface_type interface_kind(std::string_view name)
{
	static const bool steam_vr = [] {
		std::ifstream release("/etc/os-release");
		std::string line;
		bool steamos = false, vr = false;
		while (std::getline(release, line))
		{
			steamos |= line == "ID=steamos" or line == "ID=\"steamos\"";
			vr |= line == "VARIANT_ID=vr" or line == "VARIANT_ID=\"vr\"";
		}
		return steamos and vr;
	}();
	static std::map<std::string, interface_type, std::less<>> types;
	auto it = types.find(name);
	if (it == types.end())
	{
		const auto base = std::filesystem::path("/sys/class/net") / name;
		std::error_code error;
		const bool wireless = std::filesystem::exists(base / "phy80211", error) or std::filesystem::exists(base / "wireless", error);
		it = types.emplace(name, classify_interface(name, steam_vr, wireless)).first;
	}
	return it->second;
}

std::string interface_display_name(std::string_view name)
{
	const auto kind = interface_kind(name);
	std::string label;
	switch (kind)
	{
		case interface_type::wifi:
			label = _S("Wi-Fi");
			break;
		case interface_type::dongle:
			label = _S("Dongle");
			break;
		case interface_type::usb:
			label = _S("USB");
			break;
		case interface_type::unknown:
			return std::string(name);
	}
	return std::string(name) + " (" + label + ")";
}
} // namespace wivrn
