/*
 * tpt_microlog.h — C ABI for tpt-microlog (The Microscopic Telemetry & Log Router).
 *
 * Build the native library first:
 *     cargo build --release -p tpt-log-ffi
 * The static library lands in target/release/libtpt_log_ffi.a and the shared
 * library in target/release/<dll|so|dylib> per platform.
 *
 * Example (spec §6.5):
 *     tpt_log_config config = {0};
 *     config.backend = "datadog";
 *     config.api_key = "your-api-key";
 *     tpt_log_handle* log = tpt_log_create(&config);
 *     tpt_log_info(log, "User logged in", "user_id=123,ip=1.2.3.4");
 *     tpt_log_metric(log, "cpu_usage", 42.5, "host=server-01");
 *     tpt_log_shutdown(log);
 *     tpt_log_destroy(log);
 */
#ifndef TPT_MICROLOG_H
#define TPT_MICROLOG_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Error codes returned by every fallible entry point. */
enum tpt_log_code {
    TPT_LOG_OK = 0,
    TPT_LOG_NULL_ARGUMENT = 1,
    TPT_LOG_INVALID_UTF8 = 2,
    TPT_LOG_CONFIG_ERROR = 3,
    TPT_LOG_INVALID_METADATA = 4
};

/** Severity levels for tpt_log_log(). */
enum tpt_log_level {
    TPT_LOG_TRACE = 0,
    TPT_LOG_DEBUG = 1,
    TPT_LOG_INFO = 2,
    TPT_LOG_WARN = 3,
    TPT_LOG_ERROR = 4,
    TPT_LOG_FATAL = 5
};

/**
 * Logger configuration. Zero-initialize (`= {0}`) and set only what you
 * need; NULL strings and zero numbers mean "use the default".
 *
 * `wal_enabled`: -1 (or 0/anything else) default, 0 disables, 1 forces on.
 * Strings are borrowed for the duration of the call only.
 */
typedef struct tpt_log_config {
    const char* backend;         /**< "none", "http", "datadog", "elasticsearch", "cloudwatch", "prometheus", "grpc", "udp", "tcp", "file" */
    const char* endpoint;        /**< Base URL for HTTP-style backends. */
    const char* api_key;         /**< Secret token (Datadog DD-API-KEY etc.). */
    const char* service;         /**< Logical service name. */
    int32_t batch_size;          /**< Entries per batch (0 -> default 100). */
    double flush_interval;       /**< Flush interval seconds (0 -> default 1.0). */
    int32_t compression_level;   /**< zstd level (0 -> default 3). */
    int32_t max_retries;         /**< Delivery attempts (0 -> default 10). */
    int32_t wal_enabled;         /**< -1 default, 0 off, 1 on. */
    const char* wal_dir;         /**< WAL directory (NULL -> platform default). */
} tpt_log_config;

/** Opaque logger handle. Create with tpt_log_create(), destroy with tpt_log_destroy(). */
typedef struct tpt_log_handle tpt_log_handle;

/**
 * Create a logger from `config` (NULL means all defaults). Returns NULL on
 * failure; consult tpt_log_last_error() for the reason.
 */
tpt_log_handle* tpt_log_create(const tpt_log_config* config);

/** Log a TRACE message. `metadata` may be NULL ("k=v,k2=v2" pairs). */
int32_t tpt_log_trace(tpt_log_handle* handle, const char* message, const char* metadata);
/** Log a DEBUG message. */
int32_t tpt_log_debug(tpt_log_handle* handle, const char* message, const char* metadata);
/** Log an INFO message. */
int32_t tpt_log_info(tpt_log_handle* handle, const char* message, const char* metadata);
/** Log a WARN message. */
int32_t tpt_log_warn(tpt_log_handle* handle, const char* message, const char* metadata);
/** Log an ERROR message. */
int32_t tpt_log_error(tpt_log_handle* handle, const char* message, const char* metadata);
/** Log a FATAL message. */
int32_t tpt_log_fatal(tpt_log_handle* handle, const char* message, const char* metadata);
/** Log with an explicit `enum tpt_log_level` code. */
int32_t tpt_log_log(tpt_log_handle* handle, int32_t level, const char* message, const char* metadata);

/** Record a numeric metric; `tags` uses the same "k=v,..." format or NULL. */
int32_t tpt_log_metric(tpt_log_handle* handle, const char* name, double value, const char* tags);

/** Flush pending batches and drain the WAL. Idempotent. */
int32_t tpt_log_shutdown(tpt_log_handle* handle);

/** Destroy the handle (flushes implicitly). The pointer is invalid afterwards. */
void tpt_log_destroy(tpt_log_handle* handle);

/** Entries still buffered in the ingestion queue. */
size_t tpt_log_queued(tpt_log_handle* handle);

/** Library version string (static, valid for the program lifetime). */
const char* tpt_log_version(void);

/**
 * Message for the most recent failure on this thread, or NULL. The pointer
 * stays valid until the next failing call on the same thread.
 */
const char* tpt_log_last_error(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* TPT_MICROLOG_H */
