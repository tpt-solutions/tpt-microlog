# C and C++

Build the library and use the header in `tpt-log-ffi/include/tpt_microlog.h`:

```sh
cargo build --release -p tpt-log-ffi        # target/release/libtpt_log_ffi.{a,so,dylib}
cc app.c -Itpt-log-ffi/include -Ltarget/release -ltpt_log_ffi -lm -ldl -lpthread -o app
```

(On macOS add `-framework Security -framework CoreFoundation -lresolv`; on
Windows with MinGW add `-lntdll -lws2_32 -lbcrypt -luserenv`.)

```c
#include <stdio.h>
#include "tpt_microlog.h"

int main(void) {
    tpt_log_handle* log = tpt_log_create_from_toml(
        "backend = \"file\"\nfile_path = \"app.jsonl\"\nservice = \"c-app\"\n");
    if (!log) {
        fprintf(stderr, "%s\n", tpt_log_last_error());
        return 1;
    }
    tpt_log_info(log, "hello", "user_id=123,ip=\"1.2.3.4\"");  /* metadata: k=v,k2=v2 (JSON values) */
    tpt_log_metric(log, "cpu", 42.5, "host=a");
    tpt_log_shutdown(log);   /* flush + drain */
    tpt_log_destroy(log);
    return 0;
}
```

## Conventions

* Every fallible call returns a `tpt_log_code`; the message for the last
  failure on the current thread is `tpt_log_last_error()`. Pin the calling
  thread (or call it right after the failing call) if you use thread pools.
* Metadata is `"name=value,name2=value2"`; values are parsed as JSON when they
  can be (`123` is a number, `{"a":1,"b":2}` an object) and kept as strings
  otherwise. Commas inside quotes, objects or arrays do not split pairs.
* `tpt_log_stats_json` and `tpt_log_health_json` fill a caller-supplied buffer
  (use at least 1024 bytes).
* `tpt_log_set_error_callback(cb, user)` receives internal problems
  (`"delivery"`, `"wal"`, `"worker"`, `"config"`) from background threads.
* A panic inside the engine never unwinds into C: it becomes `TPT_LOG_PANIC`.
