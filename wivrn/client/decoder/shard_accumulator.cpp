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
#include "scenes/stream.h"
#include "wivrn_serialization.h"
#include "xr/instance.h"

namespace wivrn
{
void shard_accumulator::push_shard(to_headset::video_shard && shard)
{
	const auto index = shard.frame;
	const auto generation = shard.generation;
	if (index < next_frame)
		return;
	auto * frame = recovery.add(std::move(shard));
	if (not frame)
		return;
	auto [it, inserted] = feedbacks.try_emplace(index);
	auto & feedback = it->second;
	if (inserted)
		feedback = {.frame_index = index, .stream_index = stream_index,
		            .received_first_packet = instance.now()};
	feedback.generation = generation;
	if (not feedback.received_first_packet)
		feedback.received_first_packet = instance.now();
	if (frame->complete)
	{
		deserialization_packet metadata({}, frame->data.front());
		auto info = metadata.deserialize<to_headset::video_frame_info>();
		std::vector<std::span<const uint8_t>> payload;
		for (size_t i = 0; i < frame->data.size(); ++i)
			payload.push_back(frame->payload(i));
		payload.front() = payload.front().subspan(serialized_size(info));
		feedback.resent = frame->resent();
		feedback.fec_recovered = frame->recovered;
		feedback.received_last_packet = instance.now();
		feedback.sent_to_decoder = feedback.received_last_packet;
		feedback.encode_begin = info.timing_info.encode_begin;
		feedback.encode_end = info.timing_info.encode_end;
		feedback.send_begin = info.timing_info.send_begin;
		feedback.send_end = info.timing_info.send_end;
		decoder_->push_data(payload, index, false);
		decoder_->frame_completed(feedback, info.view_info);
		send_feedback(feedback);
		next_frame = index + 1;
	}
	else
	{
		if (index > next_frame + 1)
			next_frame = index - 1;
		if (index > next_frame and not recovery.frames.contains(next_frame))
		{
			recovery.frames[next_frame] = {};
			feedbacks[next_frame] = {.generation = generation, .frame_index = next_frame, .stream_index = stream_index};
		}
	}

	for (auto old = recovery.frames.begin(); old != recovery.frames.end() and old->first < next_frame;)
	{
		if (auto f = feedbacks.find(old->first); f != feedbacks.end())
		{
			if (not old->second.complete)
				send_feedback(f->second);
			feedbacks.erase(f);
		}
		old = recovery.frames.erase(old);
	}
	for (auto & [number, pending]: recovery.frames)
		if (number < index and not pending.complete)
			request_missing(number, pending);
}

void shard_accumulator::request_missing(uint64_t number, shards::receiver::frame & frame)
{
	auto now = shards::receiver::clock::now();
	if (frame.last_request != shards::receiver::clock::time_point{} and
	    now - frame.last_request < std::chrono::milliseconds(8))
		return;
	if (frame.data.empty() and frame.whole_requests >= 2)
		return;
	auto missing = recovery.missing(frame, 2);
	if (missing.empty())
	{
		if (not frame.data.empty())
			return;
		++frame.whole_requests;
		frame.last_request = now;
	}
	if (auto scene = weak_scene.lock())
		scene->send_nack({.stream = stream_index, .generation = feedbacks.at(number).generation,
		                  .frame = number, .shards = std::move(missing)});
}

void shard_accumulator::request_stalled()
{
	const auto now = shards::receiver::clock::now();
	for (auto & [number, frame]: recovery.frames)
		if (not frame.complete and now - frame.last_shard >= std::chrono::milliseconds(3))
			request_missing(number, frame);
}

void shard_accumulator::send_feedback(from_headset::feedback & feedback)
{
	if (auto scene = weak_scene.lock())
		scene->send_feedback(feedback);
}
} // namespace wivrn
