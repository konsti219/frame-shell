#pragma once
#include "decoder/shard_accumulator.h"
#include <memory>

namespace wivrn
{
// Where a decoder delivers its frames: the VR stream scene, or a frame shell desktop receiver
class frame_sink
{
public:
	virtual ~frame_sink() = default;
	virtual void push_blit_handle(shard_accumulator * decoder, std::shared_ptr<shard_accumulator::blit_handle> handle) = 0;
};
} // namespace wivrn
