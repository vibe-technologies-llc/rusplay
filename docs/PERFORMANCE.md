
# Sessions
- [S] `client-decoder-preopen`: open the client's decoder as soon as the host's `StreamFormat` arrives instead of on the first frame, so the opening keyframe does not wait for decoder set-up (rav1d's thread pool, a VAAPI session)
