/*
 * WiVRn VR streaming
 * Copyright (C) 2022-2023  Guillaume Meunier <guillaume.meunier@centraliens.net>
 * Copyright (C) 2022-2023  Patrick Nicolas <patricknicolas@laposte.net>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#include "shard_accumulator.h"
#include "fec.h"
#include "scenes/stream.h" // IWYU pragma: keep
#include "spdlog/spdlog.h"
#include "xr/instance.h"

namespace wivrn
{

using namespace wivrn::to_headset;
using shard_set = shard_accumulator::shard_set;
using data_shard = shard_accumulator::data_shard;

shard_set::shard_set(uint8_t stream_index)
{
	feedback.stream_index = stream_index;
}

void shard_set::reset(uint64_t frame_index)
{
	submitted = 0;
	data.clear();
	parity.clear();
	data_count = 0;
	requested.clear();
	last_request = 0;
	tail_requests = 0;

	uint8_t stream_index = feedback.stream_index;
	feedback = {};
	feedback.frame_index = frame_index;
	feedback.stream_index = stream_index;
}

bool shard_set::empty() const
{
	return data.empty() and parity.empty();
}

std::optional<uint16_t> shard_set::recover()
{
	if (data_count == 0 or data.size() > data_count)
		return std::nullopt;
	size_t missing = data_count - std::ranges::count_if(data, [](const auto & shard) { return shard.has_value(); });
	size_t available = std::ranges::count_if(parity, [](const auto & block) { return not block.empty(); });
	if (missing == 0 or available < missing)
		return std::nullopt;
	data.resize(data_count);
	std::vector<std::vector<uint8_t>> blocks;
	std::optional<uint16_t> first;
	for (size_t i = 0; i < data.size(); ++i)
	{
		if (data[i])
		{
			auto block = fec::block(*data[i]);
			if (block.empty()) return std::nullopt;
			blocks.push_back(std::move(block));
		}
		else
		{
			blocks.emplace_back();
			if (not first)
				first = i;
		}
	}
	if (not fec::decode(blocks, parity))
		return std::nullopt;
	for (size_t i = 0; i < data.size(); ++i)
	{
		if (data[i])
			continue;
		auto shard = fec::shard(blocks[i]);
		if (not shard or shard->shard_idx != i or shard->frame_idx != feedback.frame_index or
		    shard->stream_item_idx != feedback.stream_index)
			return std::nullopt;
		data[i] = std::move(*shard);
	}
	feedback.fec_recovered += missing;
	return first;
}

static bool is_complete(const shard_set & shards)
{
	const auto & frame = shards.data;
	if (frame.empty())
		return false;
	if (not(frame.back() and frame.back()->timing_info))
		return false;
	for (const auto & shard: frame)
		if (not shard)
			return false;
	return true;
}

std::optional<uint16_t> shard_set::insert(data_shard && shard, xr::instance & instance)
{
	if (empty())
		feedback.received_first_packet = instance.now();

	feedback.generation = shard.generation;
	auto idx = shard.shard_idx;
	if (idx >= data.size())
		data.resize(idx + 1);
	if (data[idx])
		return {};
	data[idx] = std::move(shard);
	return idx;
}

static void debug_why_not_sent(const shard_set & shards)
{
	if (shards.feedback.stream_index == 0)
		++frames_lost;
	const auto & frame = shards.data;
	if (frame.empty())
	{
		spdlog::info("frame {} was not sent because no shard was received", shards.frame_index());
		return;
	}
	int frame_idx = -1;
	size_t data = 0;
	size_t missing = 0;
	for (const auto & shard: frame)
	{
		if (shard)
		{
			frame_idx = shard->frame_idx;
			++data;
		}
		else
			++missing;
	}

	bool end = frame.back() and frame.back()->timing_info;
	spdlog::info("frame {} was not sent with {} data shards, {}{} missing", frame_idx, data, end ? "" : "at least ", missing);
}

void shard_accumulator::advance()
{
	std::swap(current, next);
	next.reset(current.frame_index() + 1);
}

void shard_accumulator::push_shard(video_stream_data_shard && shard)
{
	assert(current.frame_index() + 1 == next.frame_index());

	auto frame_diff = shard.frame_idx - current.frame_index();
	if (shard.frame_idx < current.frame_index())
	{
		// frame is in the past, drop it
		// Also shards sent again too late
		spdlog::debug("Drop shard for old frame {} (current {})", shard.frame_idx, current.frame_index());
	}
	else if (frame_diff == 0)
	{
		// A gap before this shard: the ones in between were lost
		if (shard.shard_idx > 0 and (shard.shard_idx > current.data.size() or not current.data[shard.shard_idx - 1]))
			request_missing(current, shard.shard_idx);
		auto shard_idx = current.insert(std::move(shard), instance);
		if (auto recovered = current.recover())
			shard_idx = shard_idx ? std::min(*shard_idx, *recovered) : recovered;
		try_submit_frame(shard_idx);
	}
	else if (frame_diff == 1)
	{
		// The next frame started: whatever the current one lacks won't come by itself; ask once more for what was
		// already asked for a while ago, in case that went missing too
		bool retry = current.last_request and instance.now() - current.last_request > 8'000'000;
		request_missing(current, current.data_count ? current.data_count : current.data.size(), retry ? 2 : 1, true);
		if (shard.shard_idx > 0 and (shard.shard_idx > next.data.size() or not next.data[shard.shard_idx - 1]))
			request_missing(next, shard.shard_idx);
		next.insert(std::move(shard), instance);
		next.recover();
		if (is_complete(next))
		{
			debug_why_not_sent(current);
			send_feedback(current.feedback);

			advance();

			try_submit_frame(0);
		}
	}
	else if (frame_diff == 2)
	{
		debug_why_not_sent(current);
		send_feedback(current.feedback);

		advance();

		push_shard(std::move(shard));
	}
	else
	{
		// We have lost more than one frame
		send_feedback(current.feedback);
		send_feedback(next.feedback);

		current.reset(shard.frame_idx);
		next.reset(shard.frame_idx + 1);

		push_shard(std::move(shard));
	}
}

void shard_accumulator::push_parity(video_stream_parity_shard && parity)
{
	if (parity.data_count == 0 or parity.parity_count == 0 or parity.parity_count > parity.data_count or
	    uint32_t(parity.data_count) + parity.parity_count > 65536 or parity.parity_idx >= parity.parity_count or
	    parity.payload.size() != fec::block_size or parity.frame_idx < current.frame_index())
		return;
	if (parity.frame_idx > next.frame_index())
	{
		send_feedback(current.feedback);
		send_feedback(next.feedback);
		current.reset(parity.frame_idx);
		next.reset(parity.frame_idx + 1);
	}
	auto store = [&](shard_set & set) {
		if (set.data_count and (set.data_count != parity.data_count or set.parity.size() != parity.parity_count))
			return false;
		if (set.empty())
			set.feedback.received_first_packet = instance.now();
		set.feedback.generation = parity.generation;
		set.data_count = parity.data_count;
		set.parity.resize(parity.parity_count);
		set.parity[parity.parity_idx].assign(parity.payload.begin(), parity.payload.end());
		return true;
	};
	if (parity.frame_idx == current.frame_index())
	{
		if (store(current))
		{
			try_submit_frame(current.recover());
			// Parity follows the last data shard: the frame's size is known and its tail can be asked for
			if (current.frame_index() == parity.frame_idx)
				request_missing(current, current.data_count);
		}
	}
	else if (parity.frame_idx == next.frame_index() and store(next) and next.recover() and is_complete(next))
	{
		// Like a completed next frame in push_shard: the current one is given up
		debug_why_not_sent(current);
		send_feedback(current.feedback);
		advance();
		try_submit_frame(0);
	}
}

void shard_accumulator::try_submit_frame(std::optional<uint16_t> shard_idx)
{
	if (shard_idx)
		try_submit_frame(*shard_idx);
}

void shard_accumulator::try_submit_frame(uint16_t shard_idx)
{
	auto & data_shards = current.data;
	if (shard_idx > current.submitted)
		return;
	size_t first = current.submitted;
	size_t last_idx = first;
	while (last_idx < data_shards.size() and data_shards[last_idx])
		++last_idx;
	if (first == last_idx)
		return;

	std::vector<std::span<const uint8_t>> payload;
	payload.reserve(last_idx - first);
	for (size_t idx = first; idx < last_idx; ++idx)
		payload.emplace_back(data_shards[idx]->payload);
	current.submitted = last_idx;

	bool frame_complete = last_idx == data_shards.size() and data_shards.back()->timing_info;
	decoder_->push_data(payload, data_shards[first]->frame_idx, not frame_complete);

	if (not frame_complete)
		return;

	current.feedback.resent = current.tail_requests or std::ranges::any_of(current.requested, [](uint8_t count) { return count > 0; });
	if (current.feedback.resent)
		++resend_completed;
	current.feedback.received_last_packet = instance.now();
	current.feedback.sent_to_decoder = current.feedback.received_last_packet;
	data_shard::timing_info_t timing_info = data_shards.back()->timing_info.value_or(data_shard::timing_info_t{});
	current.feedback.encode_begin = timing_info.encode_begin;
	current.feedback.encode_end = timing_info.encode_end;
	current.feedback.send_begin = timing_info.send_begin;
	current.feedback.send_end = timing_info.send_end;

	if (not data_shards.front()->view_info)
	{
		spdlog::warn("first shard has no view_info");
		return;
	}

	// Try to extract a frame
	decoder_->frame_completed(current.feedback, *data_shards.front()->view_info);

	send_feedback(current.feedback);

	advance();
}

void shard_accumulator::request_missing(shard_set & set, size_t limit, uint8_t max_requests, bool tail)
{
	limit = std::min<size_t>(limit, 65535);
	if (set.requested.size() < limit)
		set.requested.resize(limit);
	std::vector<uint16_t> missing;
	for (size_t i = 0; i < limit; ++i)
		if ((i >= set.data.size() or not set.data[i]) and set.requested[i] < max_requests)
		{
			missing.push_back(i);
			++set.requested[i];
		}
	// Neither the frame's last shard nor its parity arrived: what follows the received ones is unknown
	std::optional<uint16_t> tail_from;
	bool end_known = set.data_count or (not set.data.empty() and set.data.back() and set.data.back()->timing_info);
	if (tail and not end_known and set.tail_requests < max_requests)
	{
		tail_from = set.data.size();
		++set.tail_requests;
	}
	if (missing.empty() and not tail_from)
		return;
	set.last_request = instance.now();
	if (auto scene = weak_scene.lock())
		scene->send_nack(from_headset::video_nack{
		        .generation = set.feedback.generation,
		        .stream_item_idx = set.feedback.stream_index,
		        .frame_idx = set.frame_index(),
		        .shards = std::move(missing),
		        .tail_from = tail_from,
		});
}

void shard_accumulator::send_feedback(wivrn::from_headset::feedback & feedback)
{
	if (not feedback.received_last_packet)
		feedback.received_first_packet = instance.now();
	auto scene = weak_scene.lock();
	if (scene)
		scene->send_feedback(feedback);
}
} // namespace wivrn
