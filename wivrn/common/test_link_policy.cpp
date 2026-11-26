// SPDX-License-Identifier: GPL-3.0-or-later
#include "decode_budget.h"
#include "xor_fec.h"
#include <cstdlib>
#include <iostream>
#include <source_location>

static void check(bool value, std::source_location at = std::source_location::current())
{
	if (not value)
	{
		std::cerr << at.file_name() << ':' << at.line() << '\n';
		std::exit(1);
	}
}

int main(int argc, char **)
{
	using namespace wivrn;
	const std::array<fec_packet, 4> packets{{{1, {1, 2, 3}}, {2, {4}}, {3, {7, 8}}, {4, {9, 10, 11}}}};
	const auto parity = fec_encode(packets);
	bool finished = false;
	std::map<uint64_t, std::vector<uint8_t>> all;
	for (const auto & p: packets)
		all.emplace(p.sequence, p.bytes);
	check(not fec_recover(parity, all, &finished) and finished);
	check(not fec_recover(parity, {}, &finished) and not finished);
	check(parity.size() == 44);
	check(parity[41] == (1 ^ 4 ^ 7 ^ 9) and parity[42] == (2 ^ 8 ^ 10) and parity[43] == (3 ^ 11));
	for (size_t missing = 0; missing < packets.size(); ++missing)
	{
		std::map<uint64_t, std::vector<uint8_t>> received;
		for (size_t i = 0; i < packets.size(); ++i)
			if (i != missing)
				received.emplace(packets[i].sequence, packets[i].bytes);
		auto recovered = fec_recover(parity, received);
		if (argc > 1 and recovered)
			recovered->bytes[0] ^= 1;
		check(recovered and recovered->sequence == packets[missing].sequence and recovered->bytes == packets[missing].bytes);
		received.erase(packets[(missing + 1) % 4].sequence);
		check(not fec_recover(parity, received));
	}
	for (size_t size = 0; size < 41; ++size)
		check(not fec_recover(std::span(parity).first(size), {}));
	auto corrupt = parity;
	corrupt[0] = 255;
	check(not fec_recover(corrupt, {}));
	std::copy_n(corrupt.begin() + 1, 8, corrupt.begin() + 11);
	corrupt[0] = 4;
	check(not fec_recover(corrupt, {}));
	decode_budget budget;
	check(budget.estimate() == 2'000'000);
	for (uint64_t frame = 0; frame < 64; ++frame)
		check(budget.observe(1, frame, 5'000'000) == 6'000'000);
	check(budget.observe(1, 63, 50'000'000) == 6'000'000);
	check(budget.observe(1, 64, -1) == 6'000'000);
	check(budget.observe(1, 64, 101'000'000) == 6'000'000);
	for (uint64_t frame = 64; frame < 128; ++frame)
		budget.observe(1, frame, 1'000'000);
	check(budget.estimate() == 2'000'000);
	check(budget.observe(2, 0, 50'000'000) == 20'000'000);
	std::cout << "Erasure recovery, malformed parity, duplicate feedback and measured decode allowance passed\n";
}
