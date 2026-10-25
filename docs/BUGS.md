# Video decode
- [S] `decode-vaapi-driver-field-long-term`: after a field of a long-term frame is unmarked with MMCO 2 and a non-reference frame was decoded earlier, Mesa's VAAPI H.264 decode makes every macroblock of the next P field use reference entry 0 (software decode and the DPB state agree on the right lists, and ffmpeg's VAAPI path reproduces it); find a workaround or confirm a driver fix
