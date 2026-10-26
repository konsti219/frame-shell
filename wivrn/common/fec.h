/*
 * WiVRn VR streaming
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#pragma once

#include "wivrn_packets.h"
#include <cstdint>
#include <span>
#include <vector>

// Forward error correction over a frame's video shards: each data shard is one block of an erasure code, and parity
// shards sent after the frame let the headset rebuild missing shards without waiting for anything to be sent again.
// Losses on Wi-Fi come in runs (5 to ~100 datagrams), hence a code over the whole frame.
namespace wivrn::fec
{
// The data shards (padded) and parity are blocks of this size. Leopard wants a multiple of 64, and a datagram of
// this payload must stay within an Ethernet MTU.
constexpr size_t block_size = to_headset::video_shard::max_payload_size;

// Parity shards for a frame of `data_count` shards at `percent`, never more than its data shards
uint16_t parity_count(size_t data_count, uint8_t percent);

// Parity blocks for a frame's data blocks
std::vector<std::vector<uint8_t>> encode(std::span<const std::vector<uint8_t>> data, uint16_t parity_count);

// Rebuilds the missing (empty) data blocks from the parity blocks present (non-empty); false without enough
bool decode(std::vector<std::vector<uint8_t>> & data, std::span<const std::vector<uint8_t>> parity);
} // namespace wivrn::fec
