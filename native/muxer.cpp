#include "muxer.h"

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
}

#include <string>

namespace {

thread_local std::string last_error;

void set_error(const char *message) { last_error = message ? message : "unknown FFmpeg error"; }

void set_error_code(const char *operation, int code) {
    char message[AV_ERROR_MAX_STRING_SIZE] = {};
    av_strerror(code, message, sizeof(message));
    last_error = std::string(operation) + " failed: " + message;
}

struct Muxer {
    AVFormatContext *format = nullptr;
    AVStream *video_stream = nullptr;
    AVStream *audio_stream = nullptr;
    int fps = 30;
    int64_t last_video_pts = AV_NOPTS_VALUE;
};

void set_aac_extradata(AVCodecParameters *parameters, int sample_rate, int channels) {
    static const int sample_rates[] = {96000, 88200, 64000, 48000, 44100, 32000,
                                       24000, 22050, 16000, 12000, 11025, 8000, 7350};
    int index = 4;
    for (int i = 0; i < static_cast<int>(sizeof(sample_rates) / sizeof(sample_rates[0])); ++i) {
        if (sample_rates[i] == sample_rate) { index = i; break; }
    }
    parameters->extradata = static_cast<uint8_t *>(av_mallocz(2 + AV_INPUT_BUFFER_PADDING_SIZE));
    if (!parameters->extradata) return;
    parameters->extradata_size = 2;
    parameters->extradata[0] = static_cast<uint8_t>((2 << 3) | (index >> 1));
    parameters->extradata[1] = static_cast<uint8_t>(((index & 1) << 7) | (channels << 3));
}

void *open_muxer(const char *path, const char *format_name, int video_codec_id, int width, int height, int fps,
                 int audio_codec_id, int sample_rate, int channels) {
    auto *muxer = new Muxer();
    muxer->fps = fps;
    int result = avformat_alloc_output_context2(&muxer->format, nullptr, format_name, path);
    if (result < 0 || !muxer->format) {
        set_error_code("avformat_alloc_output_context2", result);
        delete muxer;
        return nullptr;
    }
    muxer->video_stream = avformat_new_stream(muxer->format, nullptr);
    if (!muxer->video_stream) {
        set_error("avformat_new_stream returned null");
        avformat_free_context(muxer->format);
        delete muxer;
        return nullptr;
    }
    muxer->video_stream->time_base = AVRational{1, 90000};
    muxer->video_stream->codecpar->codec_type = AVMEDIA_TYPE_VIDEO;
    muxer->video_stream->codecpar->codec_id = static_cast<AVCodecID>(video_codec_id);
    muxer->video_stream->codecpar->width = width;
    muxer->video_stream->codecpar->height = height;
    if (audio_codec_id > 0) {
        muxer->audio_stream = avformat_new_stream(muxer->format, nullptr);
        if (!muxer->audio_stream) {
            set_error("avformat_new_stream audio returned null");
            avformat_free_context(muxer->format);
            delete muxer;
            return nullptr;
        }
        muxer->audio_stream->time_base = AVRational{1, sample_rate};
        muxer->audio_stream->codecpar->codec_type = AVMEDIA_TYPE_AUDIO;
        muxer->audio_stream->codecpar->codec_id = static_cast<AVCodecID>(audio_codec_id);
        muxer->audio_stream->codecpar->sample_rate = sample_rate;
        av_channel_layout_default(&muxer->audio_stream->codecpar->ch_layout, channels);
        if (audio_codec_id == AV_CODEC_ID_AAC) set_aac_extradata(muxer->audio_stream->codecpar, sample_rate, channels);
    }
    if (!(muxer->format->oformat->flags & AVFMT_NOFILE)) {
        result = avio_open(&muxer->format->pb, path, AVIO_FLAG_WRITE);
        if (result < 0) {
            set_error_code("avio_open", result);
            avformat_free_context(muxer->format);
            delete muxer;
            return nullptr;
        }
    }
    result = avformat_write_header(muxer->format, nullptr);
    if (result < 0) {
        set_error_code("avformat_write_header", result);
        if (muxer->format->pb) avio_closep(&muxer->format->pb);
        avformat_free_context(muxer->format);
        delete muxer;
        return nullptr;
    }
    return muxer;
}

} // namespace

extern "C" void *streambox_muxer_open(const char *path, int codec_id, int width, int height,
                                       int fps) {
    if (!path || width <= 0 || height <= 0 || fps <= 0) {
        set_error("invalid MPEG-TS muxer parameters");
        return nullptr;
    }
    return open_muxer(path, "mpegts", codec_id, width, height, fps, 0, 0, 0);
}

extern "C" void *streambox_muxer_open_with_audio(const char *path, int video_codec_id, int width,
                                                   int height, int fps, int audio_codec_id,
                                                   int sample_rate, int channels) {
    if (!path || width <= 0 || height <= 0 || fps <= 0 || sample_rate <= 0 || channels <= 0) {
        set_error("invalid MPEG-TS muxer parameters");
        return nullptr;
    }
    return open_muxer(path, "mpegts", video_codec_id, width, height, fps, audio_codec_id, sample_rate, channels);
}

extern "C" void *streambox_muxer_open_target(const char *target, const char *format,
                                               int video_codec_id, int width, int height, int fps,
                                               int audio_codec_id, int sample_rate, int channels) {
    if (!target || !format || width <= 0 || height <= 0 || fps <= 0) {
        set_error("invalid network muxer parameters");
        return nullptr;
    }
    return open_muxer(target, format, video_codec_id, width, height, fps, audio_codec_id, sample_rate, channels);
}

extern "C" int streambox_muxer_write(void *opaque, const uint8_t *data, int size,
                                      int64_t pts_ms, int keyframe) {
    auto *muxer = static_cast<Muxer *>(opaque);
    if (!muxer || !data || size <= 0) { set_error("invalid video packet"); return -1; }
    AVPacket *packet = av_packet_alloc();
    if (!packet) { set_error("av_packet_alloc failed"); return -1; }
    packet->data = const_cast<uint8_t *>(data);
    packet->size = size;
    packet->pts = pts_ms * 90;
    packet->dts = packet->pts;
    if (muxer->last_video_pts != AV_NOPTS_VALUE && packet->pts <= muxer->last_video_pts) packet->pts = muxer->last_video_pts + 1;
    packet->dts = packet->pts;
    muxer->last_video_pts = packet->pts;
    packet->duration = 90000 / muxer->fps;
    packet->stream_index = muxer->video_stream->index;
    if (keyframe) packet->flags |= AV_PKT_FLAG_KEY;
    int result = av_interleaved_write_frame(muxer->format, packet);
    av_packet_free(&packet);
    if (result < 0) set_error_code("av_interleaved_write_frame", result);
    return result;
}

extern "C" int streambox_muxer_write_audio(void *opaque, const uint8_t *data, int size,
                                             int64_t pts, int duration) {
    auto *muxer = static_cast<Muxer *>(opaque);
    if (!muxer || !muxer->audio_stream || !data || size <= 0) { set_error("invalid audio packet"); return -1; }
    AVPacket *packet = av_packet_alloc();
    if (!packet) { set_error("av_packet_alloc failed"); return -1; }
    packet->data = const_cast<uint8_t *>(data);
    packet->size = size;
    packet->pts = pts;
    packet->dts = pts;
    packet->duration = duration;
    packet->stream_index = muxer->audio_stream->index;
    int result = av_interleaved_write_frame(muxer->format, packet);
    av_packet_free(&packet);
    if (result < 0) set_error_code("av_interleaved_write_frame audio", result);
    return result;
}

extern "C" void streambox_muxer_close(void *opaque) {
    auto *muxer = static_cast<Muxer *>(opaque);
    if (!muxer) return;
    if (muxer->format) {
        av_write_trailer(muxer->format);
        if (muxer->format->pb) avio_closep(&muxer->format->pb);
        avformat_free_context(muxer->format);
    }
    delete muxer;
}

extern "C" const char *streambox_muxer_last_error() { return last_error.c_str(); }

extern "C" int streambox_remux_mp4(const char *source, const char *target) {
    AVFormatContext *input = nullptr, *output = nullptr;
    AVPacket *packet = nullptr;
    int result = avformat_open_input(&input, source, nullptr, nullptr);
    if (result >= 0) result = avformat_find_stream_info(input, nullptr);
    if (result >= 0) result = avformat_alloc_output_context2(&output, nullptr, "mp4", target);
    if (result >= 0 && !output) result = AVERROR(ENOMEM);
    if (result >= 0) {
        for (unsigned i = 0; i < input->nb_streams; ++i) {
            AVStream *stream = avformat_new_stream(output, nullptr);
            if (!stream) { result = AVERROR(ENOMEM); break; }
            result = avcodec_parameters_copy(stream->codecpar, input->streams[i]->codecpar);
            if (result < 0) break;
            stream->codecpar->codec_tag = 0;
            stream->time_base = input->streams[i]->time_base;
        }
    }
    if (result >= 0) result = avio_open(&output->pb, target, AVIO_FLAG_WRITE);
    if (result >= 0) {
        AVDictionary *options = nullptr;
        av_dict_set(&options, "movflags", "+faststart", 0);
        result = avformat_write_header(output, &options);
        av_dict_free(&options);
    }
    if (result >= 0) {
        packet = av_packet_alloc();
        if (!packet) result = AVERROR(ENOMEM);
    }
    if (result >= 0) {
        while ((result = av_read_frame(input, packet)) >= 0) {
            av_packet_rescale_ts(packet, input->streams[packet->stream_index]->time_base, output->streams[packet->stream_index]->time_base);
            packet->pos = -1;
            result = av_interleaved_write_frame(output, packet);
            av_packet_unref(packet);
            if (result < 0) break;
        }
        if (result == AVERROR_EOF) result = av_write_trailer(output);
    }
    if (result < 0) set_error_code("MP4 preview remux", result);
    av_packet_free(&packet);
    if (output) {
        if (output->pb) avio_closep(&output->pb);
        avformat_free_context(output);
    }
    avformat_close_input(&input);
    return result;
}
