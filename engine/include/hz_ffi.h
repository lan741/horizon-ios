/*
 * hz_ffi.h — the C surface of the HorizonPad engine, for Swift.
 *
 * Keep this in sync with engine/crates/hz-ffi/src/lib.rs. Every fallible call
 * returns 0 on success and 1 on error; the message goes to hz_last_error().
 * Strings returned as `char *` are owned by the caller and must be released
 * with hz_string_free(). Handles are freed exactly once.
 */
#ifndef HZ_FFI_H
#define HZ_FFI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define HZ_API_VERSION 1

typedef void (*HzProgress)(void *ctx, const char *stage, uint32_t done, uint32_t total);

uint32_t hz_api_version(void);
const char *hz_last_error(void);
void hz_string_free(char *s);

/* jar -> out.hzp (+ out_report.json). cb may be NULL. */
int32_t hz_port_jar(const char *jar,
                    const char *out_hzp,
                    const char *out_report,
                    const char *minecraft_version,
                    HzProgress cb,
                    void *ctx);

/* JSON: {entries, kinds, manifest_sha256, bytes}. NULL on error. */
char *hz_pack_summary(const char *pack_path);

/* Opaque game session. */
typedef struct HzGame HzGame;

HzGame *hz_game_new(const char *pack_path);
void hz_game_free(HzGame *g);
void hz_game_tick(HzGame *g, float dt_seconds);
/* Per-section delivery: no flat copy at all.
 * hz_game_rebuild_meshes returns the revision, 0 when nothing changed.
 * hz_game_section_at fills out5 = x, y, z, vertex_count, generation. */
uint32_t hz_game_rebuild_meshes(HzGame *g);
uint32_t hz_game_section_count(const HzGame *g);
uint32_t hz_game_section_at(const HzGame *g, uint32_t index, uint32_t *out5);
uint32_t hz_game_section_vertices(const HzGame *g, int32_t x, int32_t y, int32_t z,
                                  float *out, uint32_t cap_floats);
uint32_t hz_game_flat_frame_bytes(const HzGame *g);

/* Re-bake dirty sections. Returns 1 if the vertex buffer changed, 0 if not. */
uint32_t hz_game_rebuild_frame(HzGame *g);
uint32_t hz_game_frame_quads(const HzGame *g);
/* Copies interleaved f32 vertices; returns floats written (0 if cap too small). */
uint32_t hz_game_frame_data(const HzGame *g, float *out, uint32_t cap_floats);
void hz_game_set_player(HzGame *g, float x, float y, float z, float yaw, float pitch);
uint32_t hz_game_player_pos(const HzGame *g, float *out3);
void hz_game_set_block(HzGame *g, int32_t x, int32_t y, int32_t z, uint32_t id);
/* out4 = ticks, frames, quads, non_air */
uint32_t hz_game_stats(const HzGame *g, uint32_t *out4);
/* out7 = block.xyz, normal.xyz, distance. Returns 7 on a hit, 0 on a miss. */
uint32_t hz_game_raycast(const HzGame *g,
                         float ox, float oy, float oz,
                         float dx, float dy, float dz,
                         float max_distance, float *out7);
uint32_t hz_game_air_id(const HzGame *g);

/* Block table. hz_game_block_id returns UINT32_MAX when the name is absent. */
uint32_t hz_game_block_id(const HzGame *g, const char *name);
uint32_t hz_game_block_count(const HzGame *g);
uint32_t hz_game_block_name(const HzGame *g, uint32_t index, char *out, uint32_t cap);

/* Texture table: ids + raw PNG bytes, in the order the pack numbered them. */
uint32_t hz_game_texture_count(const HzGame *g);
uint32_t hz_game_texture_name(const HzGame *g, uint32_t index, char *out, uint32_t cap);
uint32_t hz_game_texture_png_len(const HzGame *g, uint32_t index);
uint32_t hz_game_texture_png(const HzGame *g, uint32_t index, uint8_t *out, uint32_t cap);

/* Daylight for the sky channel: 0 = night, 1 = noon. */
void   hz_game_set_daylight(HzGame *g, float daylight);
float  hz_game_daylight(const HzGame *g);

/* Light queries, each 0..15. */
uint32_t hz_game_sky_at(const HzGame *g, int32_t x, int32_t y, int32_t z);
uint32_t hz_game_block_light_at(const HzGame *g, int32_t x, int32_t y, int32_t z);
uint32_t hz_game_brightness_at(const HzGame *g, int32_t x, int32_t y, int32_t z);

/* Sounds: the engine decides what plays and how loud; the platform decodes. */
uint32_t hz_game_sound_event_count(const HzGame *g);
uint32_t hz_game_sound_asset_count(const HzGame *g);
uint32_t hz_game_sound_asset_id(const HzGame *g, uint32_t index, char *out, uint32_t cap);
uint32_t hz_game_sound_len(const HzGame *g, uint32_t index);
uint32_t hz_game_sound_data(const HzGame *g, uint32_t index, uint8_t *out, uint32_t cap);
/* out4 = samples_per_second, channels, duration_ms, checksums_ok. 0 rate = not Vorbis. */
uint32_t hz_game_sound_info(const HzGame *g, uint32_t index, uint32_t *out4);
/* Returns 1 when a request was queued. */
uint32_t hz_game_play_sound(HzGame *g, const char *event, float x, float y, float z);
/* out3 = asset_index, volume, pitch. Returns 1 when one was written. */
uint32_t hz_game_next_sound(HzGame *g, float *out3);

/* Ogg Vorbis decoding through the vendored stb_vorbis. */
/* 1 when this build links the decoder; 0 means "report it, do not guess". */
uint32_t hz_vorbis_available(void);

typedef struct HzPcm HzPcm;
/* NULL on failure, reason in hz_last_error(). */
HzPcm *hz_pcm_decode(const uint8_t *data, uint32_t len);
uint32_t hz_pcm_channels(const HzPcm *pcm);
uint32_t hz_pcm_rate(const HzPcm *pcm);
uint32_t hz_pcm_frames(const HzPcm *pcm);
/* Copies frames*channels interleaved floats; 0 when the buffer is too small. */
uint32_t hz_pcm_data(const HzPcm *pcm, float *out, uint32_t cap_floats);
void hz_pcm_free(HzPcm *pcm);

/* Shader packs: the engine lists them and says what they need; it does not
 * execute them yet (see docs/SHADERS.md). */
uint32_t hz_game_shader_count(const HzGame *g);
uint32_t hz_game_shader_name(const HzGame *g, uint32_t index, char *out, uint32_t cap);
uint32_t hz_game_shader_summary(const HzGame *g, uint32_t index, char *out, uint32_t cap);
uint32_t hz_game_shader_supported(const HzGame *g, uint32_t index);
/* Translated Metal source for a pack, produced at import time. */
uint32_t hz_game_shader_msl_len(const HzGame *g, uint32_t index);
uint32_t hz_game_shader_msl(const HzGame *g, uint32_t index, uint8_t *out, uint32_t cap);
uint32_t hz_game_supported_shader_count(const HzGame *g);

int32_t hz_game_save(const HzGame *g, const char *path);
int32_t hz_game_load(HzGame *g, const char *path);

#ifdef __cplusplus
}
#endif

#endif /* HZ_FFI_H */