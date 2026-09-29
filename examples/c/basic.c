// tpt-microlog C example — mirrors spec §6.5.
//
// Build the static library, then compile against the header:
//
//   cargo build --release -p tpt-log-ffi
//   cl /I tpt-log-ffi\include examples\c\basic.c /link target\release\tpt_log_ffi.lib ...
//
// (see README for full per-platform build commands)
#include <stdio.h>
#include <string.h>

#include "tpt_microlog.h"

int main(void) {
    tpt_log_config config;
    memset(&config, 0, sizeof(config));
    config.backend = "datadog";
    config.api_key = "your-api-key";
    config.batch_size = 100;
    config.flush_interval = 1.0;

    tpt_log_handle* log = tpt_log_create(&config);
    if (log == NULL) {
        fprintf(stderr, "create failed: %s\n", tpt_log_last_error());
        return 1;
    }

    tpt_log_info(log, "User logged in", "user_id=123,ip=1.2.3.4");
    tpt_log_metric(log, "cpu_usage", 42.5, "host=server-01");
    tpt_log_error(log, "Database connection failed", "error=\"timeout\",retry_count=3");

    tpt_log_shutdown(log);
    tpt_log_destroy(log);
    return 0;
}
