// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

#include <chrono>
#include <utility>
#include <variant>

namespace wivrn
{
template <typename Receive, typename Visitor>
std::chrono::milliseconds dispatch_pending_packets(Receive && receive, Visitor && visitor, std::chrono::milliseconds timeout)
{
	while (auto packet = receive())
	{
		std::visit(std::forward<Visitor>(visitor), std::move(*packet));
		// Dispatch can create an earlier recovery deadline than the caller knew about.
		timeout = std::chrono::milliseconds::zero();
	}
	return timeout;
}
} // namespace wivrn
