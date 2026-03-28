#include <stdio.h>
#include <stdint.h>
#include "../include/ltp.h"

int main(void) {
  uint32_t v = ltp_abi_version();
  if (v != LTP_H_ABI) {
    fprintf(stderr, "ABI mismatch: ltp.h=%u lib=%u\n", LTP_H_ABI, v);
    return 2;
  }
  ltp_config cfg = {
    .bind_port = 0,
    .peer_port = 9,
    .bind_host = "127.0.0.1",
    .peer_host = "127.0.0.1",
  };
  LtpSessionHandle *s = ltp_session_create(&cfg);
  if (!s) {
    fprintf(stderr, "ltp_session_create failed, last_error=%u\n", ltp_last_error());
    return 1;
  }
  int r = ltp_send_twist_stub(s, 1, 42);
  ltp_poll_recv(s, 0);
  ltp_session_destroy(s);
  return r != 0 ? 3 : 0;
}
