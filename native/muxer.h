#ifndef STREAMBOX_MUXER_H
#define STREAMBOX_MUXER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

void *streambox_muxer_open(const char *path, int codec_id, int width, int height,
                           int fps);
void *streambox_muxer_open_with_audio(const char *path, int video_codec_id, int width,
                                      int height, int fps, int audio_codec_id,
                                      int sample_rate, int channels);
void *streambox_muxer_open_target(const char *target, const char *format,
                                  int video_codec_id, int width, int height, int fps,
                                  int audio_codec_id, int sample_rate, int channels);
int streambox_muxer_write(void *muxer, const uint8_t *data, int size,
                          int64_t pts_ms, int keyframe);
int streambox_muxer_write_audio(void *muxer, const uint8_t *data, int size,
                                int64_t pts, int duration);
void streambox_muxer_close(void *muxer);
int streambox_remux_mp4(const char *source, const char *target);
const char *streambox_muxer_last_error(void);

#ifdef __cplusplus
}
#endif

#endif
