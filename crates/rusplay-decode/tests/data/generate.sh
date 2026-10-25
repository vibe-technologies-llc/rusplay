#!/usr/bin/env sh
set -eu

cd -- "$(dirname -- "$0")"

source="-hide_banner -loglevel error -y -f lavfi -i testsrc2=size=256x144:rate=30"
aom="-c:v libaom-av1 -usage realtime -lag-in-frames 0 -cpu-used 8 -b:v 200k"
x264="-c:v libx264 -preset ultrafast -tune zerolatency -x264-params aud=1 -f h264"
x265="-c:v libx265 -preset ultrafast -tune zerolatency -x265-params aud=1:log-level=error -f hevc"

ffmpeg $source -frames:v 8 -pix_fmt yuv420p $aom av1-420-8bit.ivf
ffmpeg $source -frames:v 8 -pix_fmt yuv420p10le $aom av1-420-10bit.ivf
ffmpeg $source -frames:v 8 -pix_fmt yuv444p $aom av1-444-8bit.ivf
ffmpeg $source -frames:v 8 -pix_fmt yuv420p $x264 h264-420-8bit.h264
ffmpeg $source -frames:v 8 -pix_fmt yuv420p $x265 hevc-420-8bit.hevc
ffmpeg $source -frames:v 8 -pix_fmt yuv420p10le $x265 hevc-420-10bit.hevc
ffmpeg $source -frames:v 8 -pix_fmt yuv422p $x265 hevc-422-8bit.hevc

ffmpeg $source -frames:v 12 -pix_fmt yuv420p -c:v libx264 -preset medium \
    -x264-params aud=1:slices=4:bframes=3:ref=4:weightp=2:b-pyramid=normal \
    -f h264 h264-420-8bit-bframes.h264
ffmpeg $source -frames:v 12 -pix_fmt yuv420p -c:v libx264 -preset medium -flags +ilme+ildct \
    -x264-params aud=1:tff=1:bframes=3:ref=3:weightp=0 -f h264 h264-420-8bit-mbaff.h264
ffmpeg $source -frames:v 12 -pix_fmt yuv420p -c:v libx265 -preset medium \
    -x265-params aud=1:log-level=error:slices=4:bframes=3:ref=4:weightp=1:weightb=1:b-pyramid=1 \
    -f hevc hevc-420-8bit-bframes.hevc
ffmpeg $source -frames:v 12 -pix_fmt yuv420p -c:v libaom-av1 -cpu-used 6 -lag-in-frames 16 \
    -tiles 2x2 -b:v 200k av1-420-8bit-altref.ivf
