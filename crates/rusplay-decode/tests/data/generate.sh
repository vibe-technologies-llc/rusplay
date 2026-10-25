#!/usr/bin/env sh
set -eu

cd -- "$(dirname -- "$0")"

source="-hide_banner -loglevel error -y -f lavfi -i testsrc2=size=256x144:rate=30 -frames:v 8"
aom="-c:v libaom-av1 -usage realtime -lag-in-frames 0 -cpu-used 8 -b:v 200k"
x264="-c:v libx264 -preset ultrafast -tune zerolatency -x264-params aud=1 -f h264"
x265="-c:v libx265 -preset ultrafast -tune zerolatency -x265-params aud=1:log-level=error -f hevc"

ffmpeg $source -pix_fmt yuv420p $aom av1-420-8bit.ivf
ffmpeg $source -pix_fmt yuv420p10le $aom av1-420-10bit.ivf
ffmpeg $source -pix_fmt yuv444p $aom av1-444-8bit.ivf
ffmpeg $source -pix_fmt yuv420p $x264 h264-420-8bit.h264
ffmpeg $source -pix_fmt yuv420p $x265 hevc-420-8bit.hevc
ffmpeg $source -pix_fmt yuv420p10le $x265 hevc-420-10bit.hevc
