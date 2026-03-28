/**
 * LTP1 C ABI — stable surface for ROS 2 bridges, Unity/Unreal, Isaac Sim, and vendor SDKs.
 * Semantic version of this header MUST match `ltp_abi_version()`.
 */
#ifndef LTP_H
#define LTP_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LTP_H_ABI 1u

uint32_t ltp_abi_version(void);
uint32_t ltp_last_error(void);

typedef struct ltp_config {
  uint16_t bind_port;
  uint16_t peer_port;
  const char *bind_host;
  const char *peer_host;
} ltp_config;

typedef struct LtpSessionHandle LtpSessionHandle;

LtpSessionHandle *ltp_session_create(const ltp_config *cfg);
void ltp_session_destroy(LtpSessionHandle *p);

/**
 * Send one control datagram: body is `ControlEnvelope` (schema_id + inner_len + bytes).
 * Flushes the scheduler immediately (suitable for low-latency demos).
 */
int ltp_send_control(LtpSessionHandle *p, uint16_t stream_id, uint16_t schema_id,
                     const uint8_t *data, size_t len, uint32_t seq, uint64_t timestamp_ns);

int ltp_poll_recv(LtpSessionHandle *p, uint64_t now_ns);

/** Demo: sends SCHEMA_TWIST payload (6 floats, little-endian). */
int ltp_send_twist_stub(LtpSessionHandle *p, uint16_t stream_id, uint32_t seq);

#ifdef __cplusplus
}
#endif

#endif /* LTP_H */
