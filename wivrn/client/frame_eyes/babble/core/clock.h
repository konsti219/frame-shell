#pragma once

// Stand-in for Babble Frame Runtime's clock helpers

#include <cstdint>
#include <ctime>

namespace babble {

inline uint64_t mono_ns() {
    timespec ts{};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return uint64_t(ts.tv_sec) * 1000000000ull + uint64_t(ts.tv_nsec);
}

}
