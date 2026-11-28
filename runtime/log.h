#pragma once
#include <cstdio>
#include <cstdlib>
#include "diagnostics.h"
// The C++'s log lines, into the Rust's log (src/log.rs). LOGK is for the lines that
// trace what the title asked of the kernel or the GPU, "[k]" so they can be told from
// the rest.
//
// MW2_LOGGING=OFF compiles every call below to nothing, warnings included. The disabled
// form keeps the arguments inside an `if (false)`, so they are still type-checked and
// still count as used, and no expression in a log call may have a side effect the
// program needs.

// MW2_LOG_TIME's stamp, so that the Rust's lines and these count from the same moment.
extern "C" const char* mw2_log_stamp();

#ifdef MW2_NO_LOGGING
#   define MW2_LOG(prefix, fmt, ...) \
        do { if (false) std::fprintf(stderr, prefix fmt "\n", ##__VA_ARGS__); } while (false)
#else
#   define MW2_LOG(prefix, fmt, ...) \
        std::fprintf(stderr, "%s" prefix fmt "\n", mw2_log_stamp(), ##__VA_ARGS__)
#endif

#define LOGI(fmt, ...) MW2_LOG("[i] ", fmt, ##__VA_ARGS__)
#define LOGW(fmt, ...) MW2_LOG("[w] ", fmt, ##__VA_ARGS__)
#define LOGE(fmt, ...) MW2_LOG("[E] ", fmt, ##__VA_ARGS__)
#define LOGK(fmt, ...) MW2_LOG("[k] ", fmt, ##__VA_ARGS__)
