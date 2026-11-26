// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "network.h"
#include <memory>
#include <optional>

namespace wivrn
{
class link_probe
{
	struct implementation;
	std::unique_ptr<implementation> impl;

public:
	link_probe();
	~link_probe();
	std::optional<float> sample(const network_endpoint & local, const network_endpoint & remote);
};

class link_probe_responder
{
	struct implementation;
	std::unique_ptr<implementation> impl;

public:
	explicit link_probe_responder(uint16_t port);
	~link_probe_responder();
};
} // namespace wivrn
