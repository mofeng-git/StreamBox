extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/imgutils.h>
#include <libavutil/opt.h>
}
#include <cstdint>
#include <cstring>
#include <string>

namespace {
thread_local std::string error_message;
struct SoftwareVideo { AVCodecContext *context=nullptr; AVFrame *frame=nullptr; AVPacket *packet=nullptr; int width=0; int height=0; };
void set_error(int code) { char message[AV_ERROR_MAX_STRING_SIZE]; av_strerror(code,message,sizeof(message)); error_message=message; }
void close_video(SoftwareVideo *video) { if(!video)return; av_packet_free(&video->packet); av_frame_free(&video->frame); avcodec_free_context(&video->context); delete video; }
}
extern "C" int streambox_encoder_available(const char *name) { return name && avcodec_find_encoder_by_name(name) != nullptr; }
extern "C" const char *streambox_video_error() { return error_message.c_str(); }
extern "C" void streambox_video_close(void *raw) { close_video(static_cast<SoftwareVideo*>(raw)); }
extern "C" void *streambox_video_open(int hevc,int width,int height,int fps,int bitrate,const char *preset,const char *profile,const char *level) {
    auto *video=new SoftwareVideo();
    const AVCodec *codec=avcodec_find_encoder_by_name(hevc ? "libx265" : "libx264");
    if(!codec){error_message="software encoder is not compiled in"; close_video(video); return nullptr;}
    video->context=avcodec_alloc_context3(codec); video->frame=av_frame_alloc(); video->packet=av_packet_alloc();
    if(!video->context||!video->frame||!video->packet){error_message="allocate encoder failed";close_video(video);return nullptr;}
    video->width=width;video->height=height;
    auto *ctx=video->context;
    ctx->width=width;ctx->height=height;ctx->time_base=AVRational{1,1000};ctx->framerate=AVRational{fps,1};
    ctx->bit_rate=static_cast<int64_t>(bitrate)*1000;ctx->gop_size=fps*2;ctx->max_b_frames=0;ctx->thread_count=1;ctx->pix_fmt=AV_PIX_FMT_YUV420P;
    AVDictionary *options=nullptr;
    av_dict_set(&options,"preset",preset&&*preset?preset:"veryfast",0);
    av_dict_set(&options,"tune","zerolatency",0);
    if(profile&&*profile)av_dict_set(&options,"profile",profile,0);
    if(level&&*level)av_dict_set(&options,"level",level,0);
    if(hevc)av_dict_set(&options,"x265-params","log-level=error:pools=1:frame-threads=1",0);
    int rc=avcodec_open2(ctx,codec,&options);av_dict_free(&options);
    if(rc<0){set_error(rc);close_video(video);return nullptr;}
    video->frame->format=ctx->pix_fmt;video->frame->width=width;video->frame->height=height;
    rc=av_frame_get_buffer(video->frame,32);
    if(rc<0){set_error(rc);close_video(video);return nullptr;}
    return video;
}
using PacketCallback=void(*)(const uint8_t*,int,int64_t,int,void*);
extern "C" int streambox_video_encode(void *raw,const uint8_t *data,int size,int64_t pts,PacketCallback callback,void *opaque) {
    auto *video=static_cast<SoftwareVideo*>(raw);
    if(!video||!data||!callback||static_cast<int64_t>(size)<static_cast<int64_t>(video->width)*video->height*3/2)return AVERROR(EINVAL);
    int rc=av_frame_make_writable(video->frame);if(rc<0){set_error(rc);return rc;}
    const int width=video->width,height=video->height;
    for(int y=0;y<height;y++)std::memcpy(video->frame->data[0]+y*video->frame->linesize[0],data+y*width,width);
    const uint8_t *uv=data+width*height;
    for(int y=0;y<height/2;y++)for(int x=0;x<width/2;x++) {
        video->frame->data[1][y*video->frame->linesize[1]+x]=uv[y*width+x*2];
        video->frame->data[2][y*video->frame->linesize[2]+x]=uv[y*width+x*2+1];
    }
    video->frame->pts=pts;
    rc=avcodec_send_frame(video->context,video->frame);if(rc<0){set_error(rc);return rc;}
    while((rc=avcodec_receive_packet(video->context,video->packet))>=0) {
        callback(video->packet->data,video->packet->size,video->packet->pts,(video->packet->flags&AV_PKT_FLAG_KEY)!=0,opaque);
        av_packet_unref(video->packet);
    }
    if(rc==AVERROR(EAGAIN)||rc==AVERROR_EOF)return 0;
    set_error(rc);return rc;
}
