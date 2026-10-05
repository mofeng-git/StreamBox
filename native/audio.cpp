#include "audio.h"

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/channel_layout.h>
#include <libavutil/error.h>
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>
}

#include <alsa/asoundlib.h>
#include <algorithm>
#include <atomic>
#include <cerrno>
#include <cmath>
#include <cstring>
#include <string>
#include <vector>

struct StreamboxAudio {
    snd_pcm_t *pcm = nullptr;
    AVCodecContext *codec = nullptr;
    AVFrame *frame = nullptr;
    AVPacket *packet = nullptr;
    std::vector<int16_t> samples;
    SwrContext *resampler = nullptr;
    snd_pcm_format_t input_format = SND_PCM_FORMAT_S16_LE;
    int input_rate = 48000;
    int input_channels = 2;
    int bitrate = 128000;
    std::atomic<bool> stopping{false};
    int codec_id = 0;
    int sample_rate = 0;
    int channels = 0;
    int volume = 100;
    int frame_samples = 160;
    int64_t pts = 0;
};

namespace {

thread_local std::string last_error;

void set_error(const char *message) { last_error = message ? message : "unknown audio error"; }

void set_av_error(const char *operation, int code) {
    char message[AV_ERROR_MAX_STRING_SIZE] = {};
    av_strerror(code, message, sizeof(message));
    last_error = std::string(operation) + " failed: " + message;
}

void set_alsa_error(const char *operation, int code) {
    last_error = std::string(operation) + " failed: " + snd_strerror(code);
}

int16_t apply_volume(int16_t sample, int volume) {
    const int value = static_cast<int>(sample) * volume / 100;
    return static_cast<int16_t>(std::clamp(value, -32768, 32767));
}

uint8_t linear_to_alaw(int16_t sample) {
    int value = sample;
    int mask = 0xd5;
    if (value < 0) { value = -value - 1; mask = 0x55; }
    value = std::min(value, 32635);
    int segment = 0;
    for (int limit = 0x100; segment < 8 && value > limit; ++segment, limit <<= 1) {}
    const int mantissa = segment == 0 ? (value >> 4) : (value >> (segment + 3));
    return static_cast<uint8_t>((((segment << 4) | (mantissa & 0x0f)) ^ mask) & 0xff);
}

uint8_t linear_to_ulaw(int16_t sample) {
    int value = sample;
    const int mask = value < 0 ? 0x7f : 0xff;
    if (value < 0) value = -value;
    value = std::min(value, 32635) + 132;
    int segment = 0;
    for (int limit = 0x100; segment < 8 && value > limit; ++segment, limit <<= 1) {}
    const int mantissa = (value >> (segment + 3)) & 0x0f;
    return static_cast<uint8_t>((((segment << 4) | mantissa) ^ mask) & 0xff);
}

bool setup_pcm(StreamboxAudio *audio, const char *device) {
    int result = snd_pcm_open(&audio->pcm, device, SND_PCM_STREAM_CAPTURE, 0);
    if (result < 0) { set_alsa_error("snd_pcm_open", result); return false; }
    snd_pcm_hw_params_t *params = nullptr;
    snd_pcm_hw_params_alloca(&params);
    if ((result = snd_pcm_hw_params_any(audio->pcm, params)) < 0 ||
        (result = snd_pcm_hw_params_set_access(audio->pcm, params, SND_PCM_ACCESS_RW_INTERLEAVED)) < 0 ||
        (result = snd_pcm_hw_params_set_format(audio->pcm, params, audio->input_format)) < 0 ||
        (result = snd_pcm_hw_params_set_channels(audio->pcm, params, audio->input_channels)) < 0) {
        set_alsa_error("snd_pcm_hw_params", result);
        return false;
    }
    if ((result = snd_pcm_hw_params_set_rate(audio->pcm, params, audio->input_rate, 0)) < 0) {
        set_alsa_error("snd_pcm_hw_params_set_rate", result);
        return false;
    }
    snd_pcm_uframes_t period = static_cast<snd_pcm_uframes_t>(1024);
    snd_pcm_hw_params_set_period_size_near(audio->pcm, params, &period, nullptr);
    if ((result = snd_pcm_hw_params(audio->pcm, params)) < 0 ||
        (result = snd_pcm_prepare(audio->pcm)) < 0) {
        set_alsa_error("snd_pcm_hw_params/prepare", result);
        return false;
    }
    return true;
}

bool setup_aac(StreamboxAudio *audio) {
    const AVCodec *codec = avcodec_find_encoder(AV_CODEC_ID_AAC);
    if (!codec) { set_error("AAC encoder is not available in the bundled FFmpeg"); return false; }
    audio->codec = avcodec_alloc_context3(codec);
    if (!audio->codec) { set_error("avcodec_alloc_context3 failed"); return false; }
    audio->codec->sample_rate = audio->sample_rate;
    av_channel_layout_default(&audio->codec->ch_layout, audio->channels);
    audio->codec->sample_fmt = codec->sample_fmts ? codec->sample_fmts[0] : AV_SAMPLE_FMT_FLTP;
    audio->codec->bit_rate = audio->bitrate;
    audio->codec->time_base = AVRational{1, audio->sample_rate};
    int result = avcodec_open2(audio->codec, codec, nullptr);
    if (result < 0) { set_av_error("avcodec_open2 AAC", result); return false; }
    audio->frame_samples = audio->codec->frame_size > 0 ? audio->codec->frame_size : 1024;
    audio->frame = av_frame_alloc();
    audio->packet = av_packet_alloc();
    if (!audio->frame || !audio->packet) { set_error("audio frame allocation failed"); return false; }
    audio->frame->nb_samples = audio->frame_samples;
    audio->frame->format = audio->codec->sample_fmt;
    av_channel_layout_copy(&audio->frame->ch_layout, &audio->codec->ch_layout);
    audio->frame->sample_rate = audio->sample_rate;
    result = av_frame_get_buffer(audio->frame, 0);
    if (result < 0) { set_av_error("av_frame_get_buffer", result); return false; }
    return true;
}

bool fill_aac_frame(StreamboxAudio *audio) {
    const int count = audio->frame_samples * audio->channels;
    if (static_cast<int>(audio->samples.size()) < count) return false;
    if (av_frame_make_writable(audio->frame) < 0) return false;
    if (audio->codec->sample_fmt == AV_SAMPLE_FMT_FLTP) {
        for (int channel = 0; channel < audio->channels; ++channel) {
            auto *out = reinterpret_cast<float *>(audio->frame->data[channel]);
            for (int index = 0; index < audio->frame_samples; ++index) {
                out[index] = static_cast<float>(apply_volume(audio->samples[index * audio->channels + channel], audio->volume)) / 32768.0f;
            }
        }
    } else if (audio->codec->sample_fmt == AV_SAMPLE_FMT_S16) {
        auto *out = reinterpret_cast<int16_t *>(audio->frame->data[0]);
        for (int index = 0; index < count; ++index) out[index] = apply_volume(audio->samples[index], audio->volume);
    } else {
        set_error("bundled AAC sample format is not supported");
        return false;
    }
    audio->frame->pts = audio->pts;
    audio->pts += audio->frame_samples;
    return true;
}

} // namespace

extern "C" void streambox_audio_probe(const char *device, StreamboxAudioModeCallback callback, void *opaque) {
    snd_pcm_t *pcm = nullptr;
    if (!device || !callback || snd_pcm_open(&pcm, device, SND_PCM_STREAM_CAPTURE, SND_PCM_NONBLOCK) < 0) return;
    const char *formats[] = {"S16_LE", "S24_LE", "S24_3LE", "S32_LE", "FLOAT_LE", "U8"};
    const unsigned int rates[] = {8000,11025,16000,22050,32000,44100,48000,88200,96000,176400,192000};
    snd_pcm_hw_params_t *params = nullptr;
    snd_pcm_hw_params_alloca(&params);
    for (auto format : formats) for (auto rate : rates) for (unsigned int channels = 1; channels <= 8; ++channels) {
        if (snd_pcm_hw_params_any(pcm, params) < 0 ||
            snd_pcm_hw_params_set_access(pcm, params, SND_PCM_ACCESS_RW_INTERLEAVED) < 0 ||
            snd_pcm_hw_params_set_format(pcm, params, snd_pcm_format_value(format)) < 0 ||
            snd_pcm_hw_params_set_channels(pcm, params, channels) < 0 ||
            snd_pcm_hw_params_set_rate(pcm, params, rate, 0) < 0) continue;
        callback(rate, format, channels, opaque);
    }
    snd_pcm_close(pcm);
}

extern "C" StreamboxAudio *streambox_audio_open(const char *device, int codec, int sample_rate,
                                                  int channels, int volume, const char *input_format, int bitrate) {
    if (!device || !input_format || sample_rate <= 0 || channels <= 0 || channels > 8 || volume < 0 || volume > 200 || bitrate <= 0) {
        set_error("invalid audio parameters");
        return nullptr;
    }
    auto *audio = new StreamboxAudio();
    audio->codec_id = codec;
    audio->input_rate = sample_rate;
    audio->input_channels = channels;
    audio->input_format = snd_pcm_format_value(input_format);
    audio->sample_rate = codec == 0 ? sample_rate : 8000;
    audio->channels = codec == 0 ? channels : 1;
    audio->bitrate = bitrate;
    audio->volume = volume;
    audio->frame_samples = codec == 0 ? 1024 : 160;
    if (!setup_pcm(audio, device) || (codec == 0 && !setup_aac(audio))) {
        streambox_audio_close(audio);
        return nullptr;
    }
    AVChannelLayout input_layout, output_layout;
    av_channel_layout_default(&input_layout, audio->input_channels);
    av_channel_layout_default(&output_layout, audio->channels);
    int result = swr_alloc_set_opts2(&audio->resampler, &output_layout, AV_SAMPLE_FMT_S16, audio->sample_rate,
                                    &input_layout, AV_SAMPLE_FMT_S16, audio->input_rate, 0, nullptr);
    av_channel_layout_uninit(&input_layout);
    av_channel_layout_uninit(&output_layout);
    if (result < 0 || (result = swr_init(audio->resampler)) < 0) {
        set_av_error("audio resampler", result);
        streambox_audio_close(audio);
        return nullptr;
    }
    return audio;
}

// Normalize the advertised ALSA input formats before resampling/encoding.
int16_t read_sample(const uint8_t *data, snd_pcm_format_t format) {
    if (format == SND_PCM_FORMAT_U8) return static_cast<int16_t>((int(data[0]) - 128) * 256);
    if (format == SND_PCM_FORMAT_FLOAT_LE) {
        uint32_t bits = uint32_t(data[0]) | uint32_t(data[1]) << 8 | uint32_t(data[2]) << 16 | uint32_t(data[3]) << 24;
        float value; std::memcpy(&value, &bits, sizeof(value));
        return std::isfinite(value) ? static_cast<int16_t>(std::clamp(value, -1.0f, 1.0f) * 32767) : 0;
    }
    if (format == SND_PCM_FORMAT_S16_LE) return static_cast<int16_t>(uint16_t(data[0]) | uint16_t(data[1]) << 8);
    if (format == SND_PCM_FORMAT_S24_3LE || format == SND_PCM_FORMAT_S24_LE)
        return static_cast<int16_t>(uint16_t(data[1]) | uint16_t(data[2]) << 8);
    return static_cast<int16_t>(uint16_t(data[2]) | uint16_t(data[3]) << 8);
}

extern "C" int streambox_audio_run(StreamboxAudio *audio, StreamboxAudioPacketCallback callback,
                                    void *opaque) {
    if (!audio || !callback) { set_error("invalid audio runtime"); return -1; }
    const int bytes_per_sample = snd_pcm_format_physical_width(audio->input_format) / 8;
    if (bytes_per_sample <= 0) { set_error("unsupported audio input format"); return -1; }
    std::vector<uint8_t> raw(1024 * audio->input_channels * bytes_per_sample);
    std::vector<int16_t> input(1024 * audio->input_channels), converted, pending;
    const int packet_samples = audio->frame_samples * audio->channels;
    while (!audio->stopping.load()) {
        const snd_pcm_sframes_t frames = snd_pcm_readi(audio->pcm, raw.data(), 1024);
        if (frames == -EPIPE) { snd_pcm_prepare(audio->pcm); continue; }
        if (frames < 0) { set_alsa_error("snd_pcm_readi", static_cast<int>(frames)); return -1; }
        if (frames == 0) continue;
        for (int index = 0; index < frames * audio->input_channels; ++index)
            input[index] = read_sample(raw.data() + index * bytes_per_sample, audio->input_format);
        int capacity = static_cast<int>(av_rescale_rnd(swr_get_delay(audio->resampler, audio->input_rate) + frames,
                                                      audio->sample_rate, audio->input_rate, AV_ROUND_UP));
        converted.resize(capacity * audio->channels);
        const uint8_t *in[] = {reinterpret_cast<const uint8_t *>(input.data())};
        uint8_t *out[] = {reinterpret_cast<uint8_t *>(converted.data())};
        int count = swr_convert(audio->resampler, out, capacity, in, static_cast<int>(frames));
        if (count < 0) { set_av_error("swr_convert", count); return -1; }
        pending.insert(pending.end(), converted.begin(), converted.begin() + count * audio->channels);
        size_t consumed = 0;
        while (pending.size() - consumed >= static_cast<size_t>(packet_samples)) {
            audio->samples.assign(pending.begin() + consumed, pending.begin() + consumed + packet_samples);
            consumed += packet_samples;
            if (audio->codec_id == 0) {
                if (!fill_aac_frame(audio)) return -1;
                int result = avcodec_send_frame(audio->codec, audio->frame);
                if (result < 0) { set_av_error("avcodec_send_frame", result); return -1; }
                while ((result = avcodec_receive_packet(audio->codec, audio->packet)) >= 0) {
                    callback(audio->packet->data, audio->packet->size, audio->packet->pts, opaque);
                    av_packet_unref(audio->packet);
                }
                if (result != AVERROR(EAGAIN) && result != AVERROR_EOF) { set_av_error("avcodec_receive_packet", result); return -1; }
            } else {
                std::vector<uint8_t> encoded(audio->frame_samples);
                for (int index = 0; index < audio->frame_samples; ++index) {
                    const int16_t sample = apply_volume(audio->samples[index], audio->volume);
                    encoded[index] = audio->codec_id == 1 ? linear_to_alaw(sample) : linear_to_ulaw(sample);
                }
                callback(encoded.data(), static_cast<int>(encoded.size()), audio->pts, opaque);
                audio->pts += audio->frame_samples;
            }
        }
        pending.erase(pending.begin(), pending.begin() + consumed);
    }
    return 0;
}

extern "C" void streambox_audio_stop(StreamboxAudio *audio) {
    if (audio) audio->stopping.store(true);
}

extern "C" void streambox_audio_close(StreamboxAudio *audio) {
    if (!audio) return;
    if (audio->pcm) snd_pcm_close(audio->pcm);
    swr_free(&audio->resampler);
    if (audio->codec) avcodec_free_context(&audio->codec);
    if (audio->frame) av_frame_free(&audio->frame);
    if (audio->packet) av_packet_free(&audio->packet);
    delete audio;
}

extern "C" const char *streambox_audio_last_error() { return last_error.c_str(); }
