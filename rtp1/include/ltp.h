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

#define LTP_H_ABI 2u

/** `ltp_recv_pop` out_kind values */
#define LTP_RECV_KIND_CONTROL 0
#define LTP_RECV_KIND_VIDEO_JPEG 1

/** Inner control schemas (LeRobot interop; UTF-8 JSON payloads) */
#define LTP_SCHEMA_LEROBOT_TELEOP_JSON 0xE001u
#define LTP_SCHEMA_LEROBOT_CAMERA_JSON 0xE002u

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

/** Complete JPEG size waiting for ltp_recv_pop, or 0. Size the video pop buffer to at least this. */
size_t ltp_recv_video_pending_bytes(const LtpSessionHandle *p);

/**
 * Pop one item queued by ltp_poll_recv. For LTP_RECV_KIND_CONTROL, out_schema_id is the
 * application schema and buf receives the inner payload bytes (after ControlEnvelope).
 * Complete video frames are returned before queued control so high-rate control cannot bury JPEGs.
 * If the pending JPEG is larger than cap, control may still be popped so teleop does not stall;
 * use ltp_recv_video_pending_bytes and then pop again with a large enough buf to copy the JPEG.
 * Returns 1 on success, 0 if empty, -1 on invalid args, -2 if cap < payload (*out_len = needed).
 */
int ltp_recv_pop(LtpSessionHandle *p, int *out_kind, uint16_t *out_schema_id,
                 uint8_t *buf, size_t cap, size_t *out_len);

/**
 * End-to-end age (microseconds) of the last video frame delivered by ltp_recv_pop.
 * Computed as (wall_now - header.timestamp_ns) at pop time.  0 if no video delivered yet.
 * Use for display-side latency overlay.
 */
uint64_t ltp_recv_last_video_age_us(const LtpSessionHandle *p);

/** Demo: sends SCHEMA_TWIST payload (6 floats, little-endian). */
int ltp_send_twist_stub(LtpSessionHandle *p, uint16_t stream_id, uint32_t seq);

#ifdef __cplusplus
}
#endif

#endif /* LTP_H */
