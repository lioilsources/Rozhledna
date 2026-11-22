// C ABI of rz-hub-ffi (see src/lib.rs for the semantics). Ids are uint64_t; 0 = none / error.
#pragma once
#include <stdbool.h>
#include <stdint.h>

typedef struct RzHub RzHub;
typedef void (*RzFrameCallback)(void *ctx, uint64_t tile);
typedef struct { float x, y; bool visible; uint32_t shape_id; } RzCursor;

RzHub *rz_hub_new(const char *state_dir, const char *name, RzFrameCallback on_frame, void *ctx);
void rz_hub_free(RzHub *h);
char *rz_hub_fingerprint(RzHub *h);
char *rz_hub_next_event(RzHub *h, uint32_t timeout_ms);   // JSON or NULL
void rz_hub_free_string(char *s);
bool rz_hub_start_discovery(RzHub *h);
uint64_t rz_hub_add_agent(RzHub *h, const char *addr);
void rz_hub_remove_agent(RzHub *h, uint64_t agent);
uint64_t rz_hub_subscribe(RzHub *h, uint64_t agent, uint64_t source, uint32_t max_w, uint32_t max_h, uint32_t fps,
                          bool focus, uint32_t bitrate_kbps);
bool rz_hub_update_tile(RzHub *h, uint64_t tile, uint64_t source, uint32_t max_w, uint32_t max_h, uint32_t fps,
                        bool focus, uint32_t bitrate_kbps);
void rz_hub_unsubscribe(RzHub *h, uint64_t tile);
void *rz_hub_take_frame(RzHub *h, uint64_t tile);          // +1 CVPixelBufferRef or NULL
void rz_hub_key(RzHub *h, uint64_t tile, uint32_t hid, bool down);
bool rz_hub_key_mac(RzHub *h, uint64_t tile, uint16_t keycode, bool down);  // grab: kVK_* code
void rz_hub_button(RzHub *h, uint64_t tile, uint32_t button, bool down, float x, float y, uint32_t clicks);
void rz_hub_scroll(RzHub *h, uint64_t tile, float dx, float dy, bool precise, uint32_t phase);
void rz_hub_text(RzHub *h, uint64_t tile, const char *utf8);
void rz_hub_pointer(RzHub *h, uint64_t tile, float x, float y);
void rz_hub_release_all(RzHub *h, uint64_t tile);
bool rz_hub_cursor(RzHub *h, uint64_t tile, RzCursor *out);
void rz_hub_answer(RzHub *h, uint64_t request, bool accept);
bool rz_hub_forget(RzHub *h, const char *fingerprint);
