#pragma once

#include <stdint.h>

typedef struct StreamboxAudio StreamboxAudio;
typedef void (*StreamboxAudioPacketCallback)(const uint8_t *data, int size,
                                             int64_t pts, void *opaque);

#ifdef __cplusplus
extern "C" {
#endif
typedef void (*StreamboxAudioModeCallback)(unsigned int rate, const char *format, unsigned int channels, void *opaque);
void streambox_audio_probe(const char *device, StreamboxAudioModeCallback callback, void *opaque);
StreamboxAudio *streambox_audio_open(const char *device, int codec, int sample_rate,
                                     int channels, int volume, const char *input_format, int bitrate);
int streambox_audio_run(StreamboxAudio *audio, StreamboxAudioPacketCallback callback,
                        void *opaque);
void streambox_audio_stop(StreamboxAudio *audio);
void streambox_audio_close(StreamboxAudio *audio);
const char *streambox_audio_last_error(void);
#ifdef __cplusplus
}
#endif
