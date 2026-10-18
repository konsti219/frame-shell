#pragma once

// Stand-in for Babble Frame Runtime's logger: printf-style messages to spdlog

#include <cstdarg>
#include <cstdio>
#include <spdlog/spdlog.h>

namespace babble::log {

class Logger {
public:
    explicit Logger(const char *module) : module_(module) {}

    void debug(const char *fmt, ...) const __attribute__((format(printf, 2, 3))) { va_list a; va_start(a, fmt); write(spdlog::level::debug, fmt, a); va_end(a); }
    void info(const char *fmt, ...) const __attribute__((format(printf, 2, 3))) { va_list a; va_start(a, fmt); write(spdlog::level::info, fmt, a); va_end(a); }
    void warn(const char *fmt, ...) const __attribute__((format(printf, 2, 3))) { va_list a; va_start(a, fmt); write(spdlog::level::warn, fmt, a); va_end(a); }
    void error(const char *fmt, ...) const __attribute__((format(printf, 2, 3))) { va_list a; va_start(a, fmt); write(spdlog::level::err, fmt, a); va_end(a); }

private:
    void write(spdlog::level::level_enum level, const char *fmt, va_list args) const {
        char buf[512];
        vsnprintf(buf, sizeof(buf), fmt, args);
        spdlog::log(level, "Eyes ({}): {}", module_, buf);
    }
    const char *module_;
};

}
