#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Video,
    Audio,
    Network,
    Input,
    Capture,
    Scaling,
    Color,
}

impl Section {
    pub const ALL: [Self; 7] = [
        Self::Video,
        Self::Audio,
        Self::Network,
        Self::Input,
        Self::Capture,
        Self::Scaling,
        Self::Color,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Network => "Network",
            Self::Input => "Input",
            Self::Capture => "Capture",
            Self::Scaling => "Scaling",
            Self::Color => "Colour",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Flag,
    Integer { min: i64, max: i64 },
    Decimal,
    Choice(&'static [&'static str]),
    Primaries,
    MultiChoice(&'static [&'static str]),
    Text,
    Address,
    Resolution,
    FrameRate,
    Pairs,
}

#[derive(Debug, Clone, Copy)]
pub struct Condition {
    pub path: &'static str,
    pub any_of: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub section: Section,
    pub audience: Audience,
    pub group: &'static str,
    pub path: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub kind: Kind,
    pub optional: bool,
    pub when: &'static [Condition],
}

const fn unsigned(max: i64) -> Kind {
    Kind::Integer { min: 0, max }
}

const U8: Kind = unsigned(u8::MAX as i64);
const U16: Kind = unsigned(u16::MAX as i64);
const U32: Kind = unsigned(u32::MAX as i64);
const HDR_NITS: Kind = Kind::Integer {
    min: 1,
    max: 10_000,
};
const POSITIVE_U32: Kind = Kind::Integer {
    min: 1,
    max: u32::MAX as i64,
};

const NICE: Kind = Kind::Integer { min: -20, max: 19 };
const RTKIT_PRIORITY: Kind = Kind::Integer { min: 0, max: 99 };
const BACKOFF_PERCENT: Kind = Kind::Integer { min: 1, max: 90 };

const CODECS: &[&str] = &["auto", "h264", "hevc", "av1"];
const CHROMA: &[&str] = &["auto", "4:2:0", "4:2:2", "4:4:4"];
const BIT_DEPTHS: &[&str] = &["auto", "eight", "ten", "twelve"];
const COLOR_MODES: &[&str] = &["source", "configured"];
pub const PRIMARIES: &[&str] = &[
    "bt709",
    "display-p3",
    "dci-p3",
    "bt2020",
    "bt601-525",
    "bt601-625",
];
const TRANSFERS: &[&str] = &["bt709", "srgb", "gamma22", "pq", "hlg", "linear"];
const MATRICES: &[&str] = &["bt709", "bt601", "bt2020-ncl"];
const TONE_OPERATORS: &[&str] = &["bt2390", "bt2446a", "spline", "reinhard", "clip"];
const GAMUT_MODES: &[&str] = &[
    "perceptual",
    "relative",
    "absolute",
    "saturation",
    "desaturate",
    "highlight",
];
const RANGES: &[&str] = &["limited", "full"];
const LOSS_RECOVERIES: &[&str] = &["reference", "keyframe"];
const PRESENT_MODES: &[&str] = &["mailbox", "immediate", "fifo"];
const PRESENT_PACINGS: &[&str] = &["off", "latency", "smooth"];
const OUTPUT_COLOR_SPACES: &[&str] = &["auto", "srgb", "display-p3", "extended-linear", "pq"];
const OUTPUT_FORMATS: &[&str] = &["auto", "rgba8", "rgb10a2", "rgba16-float"];
const DITHER_MODES: &[&str] = &["blue-noise", "ordered", "off"];
const COLOR_METHODS: &[&str] = &["analytic", "lut"];
const LUT_SIZES: Kind = Kind::Integer { min: 17, max: 129 };
const BACKENDS: &[&str] = &["auto", "vaapi", "nvenc", "x264", "x265", "svt-av1"];
const DECODER_BACKENDS: &[&str] = &["auto", "vaapi", "nvdec", "software"];
const RATE_CONTROL_MODES: &[&str] = &["cbr", "vbr", "constant-quality"];
const NVENC_PRESETS: &[&str] = &["p1", "p2", "p3", "p4", "p5", "p6", "p7"];
const NVENC_TUNINGS: &[&str] = &["ultra-low-latency", "low-latency", "high-quality"];
const SOFTWARE_PRESETS: &[&str] = &[
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
    "placebo",
];
const X264_TUNES: &[&str] = &[
    "zerolatency",
    "film",
    "animation",
    "grain",
    "stillimage",
    "psnr",
    "ssim",
    "fastdecode",
];
const X265_TUNES: &[&str] = &[
    "zerolatency",
    "grain",
    "animation",
    "psnr",
    "ssim",
    "fastdecode",
];
const AUDIO_CODECS: &[&str] = &["opus", "flac"];
const SAMPLE_RATES: &[&str] = &["48000", "96000"];
const CHANNEL_LAYOUTS: &[&str] = &["mono", "stereo", "5.1", "7.1"];
const OPUS_FRAME_DURATIONS: &[&str] = &["2.5ms", "5ms", "10ms", "20ms"];
const SVT_AV1_LOG_LEVELS: &[&str] = &["off", "fatal", "error", "warn", "info", "debug"];
const POINTER_MODES: &[&str] = &["absolute", "relative", "auto"];
const GAMEPAD_PERSONALITIES: &[&str] = &["xbox360", "dualsense", "dualshock4"];
const CLIPBOARD_DIRECTIONS: &[&str] = &["off", "client-to-host", "host-to-client", "both"];
const CLIPBOARD_BYTES: Kind = Kind::Integer {
    min: 1,
    max: 1_048_576,
};
const RELEASE_CHORDS: &[&str] = &["ctrl-alt", "ctrl-shift", "alt-shift", "ctrl-alt-shift"];
const HOTKEY_KEYS: &[&str] = &[
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10",
    "f11", "f12",
];
const CHROMA_SITINGS: &[&str] = &["left", "center"];
const SOURCE_KINDS: &[&str] = &["monitor", "window", "any", "virtual"];
const CURSOR_MODES: &[&str] = &["hidden", "embedded", "metadata"];
const SCALE_FILTERS: &[&str] = &[
    "bilinear",
    "hermite",
    "catmull-rom",
    "mitchell",
    "lanczos2",
    "lanczos3",
    "lanczos4",
    "ewa-lanczos",
    "ewa-lanczos-sharp",
    "ewa-ginseng",
    "ewa-hann",
    "ewa-robidoux",
    "ewa-robidoux-sharp",
];

const BACKEND_PATH: &str = "video.encoder.backend";
const MODE_PATH: &str = "video.encoder.rate_control.mode";

const DECODER_BACKEND_PATH: &str = "video.decoder.backend";

const VAAPI_DECODER: &[Condition] = &[Condition {
    path: DECODER_BACKEND_PATH,
    any_of: &["vaapi"],
}];
const NVDEC_DECODER: &[Condition] = &[Condition {
    path: DECODER_BACKEND_PATH,
    any_of: &["nvdec"],
}];
const SOFTWARE_DECODER: &[Condition] = &[Condition {
    path: DECODER_BACKEND_PATH,
    any_of: &["software"],
}];
const VAAPI: &[Condition] = &[Condition {
    path: BACKEND_PATH,
    any_of: &["vaapi"],
}];
const NVENC: &[Condition] = &[Condition {
    path: BACKEND_PATH,
    any_of: &["nvenc"],
}];
const X264: &[Condition] = &[Condition {
    path: BACKEND_PATH,
    any_of: &["x264"],
}];
const X265: &[Condition] = &[Condition {
    path: BACKEND_PATH,
    any_of: &["x265"],
}];
const SVT_AV1: &[Condition] = &[Condition {
    path: BACKEND_PATH,
    any_of: &["svt-av1"],
}];
const SOFTWARE_X26X: &[Condition] = &[Condition {
    path: BACKEND_PATH,
    any_of: &["x264", "x265"],
}];
const SOFTWARE_THREADED: &[Condition] = &[Condition {
    path: BACKEND_PATH,
    any_of: &["x264", "x265", "svt-av1"],
}];
const HARDWARE_RATE_CONTROL: &[Condition] = &[Condition {
    path: BACKEND_PATH,
    any_of: &["auto", "vaapi", "nvenc"],
}];
const VARIABLE_BITRATE: &[Condition] = &[
    Condition {
        path: BACKEND_PATH,
        any_of: &["auto", "vaapi", "nvenc"],
    },
    Condition {
        path: MODE_PATH,
        any_of: &["vbr"],
    },
];
const CONSTANT_QUALITY: &[Condition] = &[
    Condition {
        path: BACKEND_PATH,
        any_of: &["auto", "vaapi", "nvenc"],
    },
    Condition {
        path: MODE_PATH,
        any_of: &["constant-quality"],
    },
];

const fn field(
    audience: Audience,
    section: Section,
    group: &'static str,
    path: &'static str,
    label: &'static str,
    kind: Kind,
) -> Field {
    Field {
        audience,
        section,
        group,
        path,
        label,
        unit: "",
        kind,
        optional: false,
        when: &[],
    }
}

impl Field {
    const fn unit(mut self, unit: &'static str) -> Self {
        self.unit = unit;
        self
    }

    const fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    const fn when(mut self, when: &'static [Condition]) -> Self {
        self.when = when;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    Shared,
    Host,
    Client,
}

impl Audience {
    pub const fn allows(self, host: bool) -> bool {
        match self {
            Self::Shared => true,
            Self::Host => host,
            Self::Client => !host,
        }
    }
}

pub fn audience(path: &str) -> Audience {
    FIELDS
        .iter()
        .find(|field| field.path == path)
        .map_or(Audience::Shared, |field| field.audience)
}

pub static FIELDS: &[Field] = &[
    field(
        Audience::Host,
        Section::Video,
        "Stream",
        "video.resolution",
        "Resolution",
        Kind::Resolution,
    )
    .optional(),
    field(
        Audience::Shared,
        Section::Video,
        "Stream",
        "video.frame_rate",
        "Frame rate",
        Kind::FrameRate,
    )
    .unit("fps"),
    field(
        Audience::Host,
        Section::Video,
        "Stream",
        "video.codec",
        "Codec",
        Kind::Choice(CODECS),
    ),
    field(
        Audience::Host,
        Section::Video,
        "Stream",
        "video.auto_detect_wait_ms",
        "Wait for the source",
        Kind::Integer {
            min: 0,
            max: 10_000,
        },
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Video,
        "Stream",
        "video.bitrate_kbps",
        "Bitrate",
        U32,
    )
    .unit("kbps"),
    field(
        Audience::Host,
        Section::Video,
        "Stream",
        "video.vbv_frames",
        "Rate buffer",
        U8,
    )
    .unit("frames"),
    field(
        Audience::Host,
        Section::Video,
        "Stream",
        "video.quality_interval_ms",
        "Quality sample interval",
        Kind::Integer {
            min: 0,
            max: 60_000,
        },
    )
    .unit("ms"),
    field(
        Audience::Host,
        Section::Video,
        "Pixel format",
        "video.pixel_format.chroma",
        "Chroma subsampling",
        Kind::Choice(CHROMA),
    ),
    field(
        Audience::Host,
        Section::Video,
        "Pixel format",
        "video.pixel_format.depth",
        "Bit depth",
        Kind::Choice(BIT_DEPTHS),
    ),
    field(
        Audience::Host,
        Section::Video,
        "Colour",
        "video.color_mode",
        "Stream colour",
        Kind::Choice(COLOR_MODES),
    ),
    field(
        Audience::Host,
        Section::Video,
        "Colour",
        "video.color.primaries",
        "Primaries",
        Kind::Primaries,
    ),
    field(
        Audience::Host,
        Section::Video,
        "Colour",
        "video.color.transfer",
        "Transfer function",
        Kind::Choice(TRANSFERS),
    ),
    field(
        Audience::Host,
        Section::Video,
        "Colour",
        "video.color.matrix",
        "Y'CbCr matrix",
        Kind::Choice(MATRICES),
    )
    .optional(),
    field(
        Audience::Host,
        Section::Video,
        "Colour",
        "video.color.range",
        "Range",
        Kind::Choice(RANGES),
    ),
    field(
        Audience::Host,
        Section::Video,
        "Colour",
        "video.color.hdr.reference_white_nits",
        "HDR reference white",
        HDR_NITS,
    )
    .unit("nits"),
    field(
        Audience::Host,
        Section::Video,
        "Colour",
        "video.color.hdr.peak_nits",
        "HDR peak luminance",
        HDR_NITS,
    )
    .unit("nits"),
    field(
        Audience::Host,
        Section::Video,
        "HDR metadata",
        "video.color.hdr.mastering.primaries",
        "Mastering display primaries",
        Kind::Primaries,
    )
    .optional(),
    field(
        Audience::Host,
        Section::Video,
        "HDR metadata",
        "video.color.hdr.mastering.max_nits",
        "Mastering display peak",
        Kind::Decimal,
    )
    .unit("nits")
    .optional(),
    field(
        Audience::Host,
        Section::Video,
        "HDR metadata",
        "video.color.hdr.mastering.min_nits",
        "Mastering display black",
        Kind::Decimal,
    )
    .unit("nits")
    .optional(),
    field(
        Audience::Host,
        Section::Video,
        "HDR metadata",
        "video.color.hdr.content_light.max_cll_nits",
        "MaxCLL",
        U16,
    )
    .unit("nits")
    .optional(),
    field(
        Audience::Host,
        Section::Video,
        "HDR metadata",
        "video.color.hdr.content_light.max_fall_nits",
        "MaxFALL",
        U16,
    )
    .unit("nits")
    .optional(),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        BACKEND_PATH,
        "Backend",
        Kind::Choice(BACKENDS),
    ),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.device",
        "Device",
        Kind::Text,
    )
    .when(VAAPI),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.low_power",
        "Low power mode",
        Kind::Flag,
    )
    .when(VAAPI),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.gpu_index",
        "GPU index",
        U32,
    )
    .when(NVENC),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.preset",
        "Preset",
        Kind::Choice(NVENC_PRESETS),
    )
    .when(NVENC),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.tuning",
        "Tuning",
        Kind::Choice(NVENC_TUNINGS),
    )
    .when(NVENC),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        MODE_PATH,
        "Rate control",
        Kind::Choice(RATE_CONTROL_MODES),
    )
    .when(HARDWARE_RATE_CONTROL),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.rate_control.max_bitrate_kbps",
        "Maximum bitrate",
        U32,
    )
    .unit("kbps")
    .when(VARIABLE_BITRATE),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.rate_control.qp",
        "Quantizer",
        U8,
    )
    .when(CONSTANT_QUALITY),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.preset",
        "Preset",
        Kind::Choice(SOFTWARE_PRESETS),
    )
    .when(SOFTWARE_X26X),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.tune",
        "Tune",
        Kind::MultiChoice(X264_TUNES),
    )
    .when(X264),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.tune",
        "Tune",
        Kind::Choice(X265_TUNES),
    )
    .optional()
    .when(X265),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.preset",
        "Preset",
        U8,
    )
    .when(SVT_AV1),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.low_latency",
        "Low latency",
        Kind::Flag,
    )
    .when(SVT_AV1),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.reconstruction",
        "Reconstruction for quality metrics",
        Kind::Flag,
    )
    .when(SVT_AV1),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.log_level",
        "Log level",
        Kind::Choice(SVT_AV1_LOG_LEVELS),
    )
    .when(SVT_AV1),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.threads",
        "Threads",
        POSITIVE_U32,
    )
    .optional()
    .when(SOFTWARE_THREADED),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder.params",
        "Parameters",
        Kind::Pairs,
    ),
    field(
        Audience::Host,
        Section::Video,
        "Encoder",
        "video.encoder_gpu_handoff",
        "Hand frames to the encoder on the GPU",
        Kind::Flag,
    ),
    field(
        Audience::Client,
        Section::Video,
        "Decoder",
        DECODER_BACKEND_PATH,
        "Backend",
        Kind::Choice(DECODER_BACKENDS),
    ),
    field(
        Audience::Client,
        Section::Video,
        "Decoder",
        "video.decode_queue_frames",
        "Decode queue",
        U8,
    )
    .unit("frames"),
    field(
        Audience::Client,
        Section::Video,
        "Decoder",
        "video.decoder.device",
        "Device",
        Kind::Text,
    )
    .when(VAAPI_DECODER),
    field(
        Audience::Client,
        Section::Video,
        "Decoder",
        "video.decoder.copy_output",
        "Copy VAAPI output through CPU (4:2:0)",
        Kind::Flag,
    )
    .when(VAAPI_DECODER),
    field(
        Audience::Client,
        Section::Video,
        "Decoder",
        "video.decoder.gpu_index",
        "GPU index",
        U32,
    )
    .when(NVDEC_DECODER),
    field(
        Audience::Client,
        Section::Video,
        "Decoder",
        "video.decoder.threads",
        "Threads",
        POSITIVE_U32,
    )
    .optional()
    .when(SOFTWARE_DECODER),
    field(
        Audience::Client,
        Section::Video,
        "Presentation",
        "video.present_mode",
        "Present mode",
        Kind::Choice(PRESENT_MODES),
    ),
    field(
        Audience::Client,
        Section::Video,
        "Presentation",
        "video.present_frame_latency",
        "Frame latency",
        Kind::Integer { min: 1, max: 3 },
    )
    .unit("frames"),
    field(
        Audience::Client,
        Section::Video,
        "Presentation",
        "video.present_pacing",
        "Pacing",
        Kind::Choice(PRESENT_PACINGS),
    ),
    field(
        Audience::Client,
        Section::Video,
        "Presentation",
        "video.present_margin_us",
        "Margin before the display refresh",
        Kind::Integer {
            min: 0,
            max: 50_000,
        },
    )
    .unit("us"),
    field(
        Audience::Client,
        Section::Video,
        "Presentation",
        "video.present_jitter_us",
        "Jitter buffer",
        Kind::Integer {
            min: 0,
            max: 100_000,
        },
    )
    .unit("us"),
    field(
        Audience::Client,
        Section::Video,
        "Presentation",
        "video.stats_overlay",
        "Statistics over the picture",
        Kind::Flag,
    ),
    field(
        Audience::Client,
        Section::Video,
        "Presentation",
        "video.quality_metrics",
        "Encode quality metrics",
        Kind::Flag,
    ),
    field(
        Audience::Client,
        Section::Video,
        "Presentation",
        "video.display_debounce_ms",
        "Window resize settle time",
        Kind::Integer { min: 0, max: 5_000 },
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.codec",
        "Codec",
        Kind::Choice(AUDIO_CODECS),
    ),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.sample_rate",
        "Sample rate",
        Kind::Choice(SAMPLE_RATES),
    )
    .unit("Hz"),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.channels",
        "Channels",
        Kind::Choice(CHANNEL_LAYOUTS),
    ),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.jitter_buffer_ms",
        "Jitter buffer",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.jitter_buffer_max_ms",
        "Adaptive jitter ceiling",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.jitter_relax_ms",
        "Jitter relax interval",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.rate_match_max_ppm",
        "Clock drift correction limit",
        U32,
    )
    .unit("ppm"),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.decode_queue_packets",
        "Decode queue",
        U16,
    )
    .unit("packets"),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.max_conceal_ms",
        "Concealment limit",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Audio,
        "Stream",
        "audio.fec_overhead_percent",
        "FEC overhead",
        U8,
    )
    .unit("%"),
    field(
        Audience::Shared,
        Section::Audio,
        "Opus",
        "audio.opus.bitrate_kbps",
        "Bitrate",
        U32,
    )
    .unit("kbps"),
    field(
        Audience::Shared,
        Section::Audio,
        "Opus",
        "audio.opus.frame_duration",
        "Frame duration",
        Kind::Choice(OPUS_FRAME_DURATIONS),
    ),
    field(
        Audience::Shared,
        Section::Audio,
        "Opus",
        "audio.opus.inband_fec",
        "In-band FEC",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Audio,
        "FLAC",
        "audio.flac.compression_level",
        "Compression level",
        U8,
    ),
    field(
        Audience::Shared,
        Section::Audio,
        "FLAC",
        "audio.flac.block_size",
        "Block size",
        U16,
    )
    .unit("samples"),
    field(
        Audience::Shared,
        Section::Audio,
        "Microphone",
        "audio.microphone.enabled",
        "Send microphone",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Audio,
        "Microphone",
        "audio.microphone.bitrate_kbps",
        "Bitrate",
        U32,
    )
    .unit("kbps"),
    field(
        Audience::Shared,
        Section::Audio,
        "Microphone",
        "audio.microphone.frame_duration",
        "Frame duration",
        Kind::Choice(OPUS_FRAME_DURATIONS),
    ),
    field(
        Audience::Shared,
        Section::Audio,
        "Microphone",
        "audio.microphone.inband_fec",
        "In-band FEC",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Audio,
        "Microphone",
        "audio.microphone.jitter_buffer_ms",
        "Jitter buffer",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Audio,
        "Microphone",
        "audio.microphone.fec_overhead_percent",
        "FEC overhead",
        U8,
    )
    .unit("%"),
    field(
        Audience::Shared,
        Section::Network,
        "Socket",
        "network.bind_address",
        "Bind address",
        Kind::Address,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Socket",
        "network.port",
        "Port",
        U16,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Socket",
        "network.socket_buffer_bytes",
        "Socket buffer",
        U32,
    )
    .unit("bytes"),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.mtu",
        "MTU",
        U16,
    )
    .unit("bytes"),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.fec_overhead_percent",
        "FEC overhead",
        U8,
    )
    .unit("%"),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.pacing",
        "Pacing",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.pacing_window_percent",
        "Pacing window",
        U8,
    )
    .unit("%"),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.pacing_bitrate_multiple",
        "Pacing rate, times the bitrate",
        U8,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.pacing_max_mbps",
        "Pacing rate cap",
        U32,
    )
    .unit("Mbps"),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.frame_thread_nice",
        "Frame thread nice value",
        NICE,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.rtkit_priority",
        "Realtime priority through rtkit",
        RTKIT_PRIORITY,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.send_timer_slack_us",
        "Send and receive thread timer slack",
        U32,
    )
    .unit("us"),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.reassembly_timeout_ms",
        "Reassembly timeout",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.reassembly_max_pending_frames",
        "Reassembly pending frames",
        U8,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Transport",
        "network.reassembly_max_fragments",
        "Reassembly fragment limit",
        U16,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Control",
        "network.control_retransmit_ms",
        "Retransmit interval",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Control",
        "network.control_max_retransmits",
        "Max retransmits",
        U8,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Discovery",
        "network.discovery_port",
        "Discovery port",
        U16,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Discovery",
        "network.discovery_group",
        "Multicast group",
        Kind::Address,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Discovery",
        "network.discovery_scan_ms",
        "Scan time",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Pairing",
        "network.pairing_pin_digits",
        "PIN digits",
        U8,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Pairing",
        "network.pairing_max_attempts",
        "Max attempts",
        U8,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Pairing",
        "network.pairing_timeout_ms",
        "Pairing timeout",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Pairing",
        "network.reconnect_timeout_ms",
        "Reconnect timeout",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.handshake_timeout_ms",
        "Handshake timeout",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.peer_timeout_ms",
        "Peer timeout",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.keepalive_interval_ms",
        "Keepalive interval",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.keyframe_request_interval_ms",
        "Keyframe request interval",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.loss_recovery",
        "Loss recovery",
        Kind::Choice(LOSS_RECOVERIES),
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.frame_ack_interval_ms",
        "Frame acknowledgement interval",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.cursor_image_repeat_ms",
        "Cursor image repeat",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.stats_log_interval_ms",
        "Stats log interval",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.stats_json",
        "JSON statistics on stdout",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Bitrate adaptation",
        "network.adaptive_bitrate",
        "Adaptive bitrate",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Network,
        "Bitrate adaptation",
        "network.adaptive_bitrate_floor_kbps",
        "Floor",
        POSITIVE_U32,
    )
    .unit("kbps"),
    field(
        Audience::Shared,
        Section::Network,
        "Bitrate adaptation",
        "network.adaptive_bitrate_step_kbps",
        "Increase step",
        POSITIVE_U32,
    )
    .unit("kbps"),
    field(
        Audience::Shared,
        Section::Network,
        "Bitrate adaptation",
        "network.adaptive_bitrate_window_ms",
        "Clean window",
        POSITIVE_U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Session",
        "network.clock_sync_interval_ms",
        "Clock sync interval",
        U32,
    )
    .unit("ms"),
    field(
        Audience::Shared,
        Section::Network,
        "Bitrate adaptation",
        "network.adaptive_bitrate_backoff_percent",
        "Backoff",
        BACKOFF_PERCENT,
    )
    .unit("%"),
    field(
        Audience::Shared,
        Section::Network,
        "Bitrate adaptation",
        "network.bitrate_hint_min_kbps",
        "Host hint minimum",
        POSITIVE_U32,
    )
    .unit("kbps"),
    field(
        Audience::Shared,
        Section::Network,
        "Bitrate adaptation",
        "network.bitrate_hint_max_kbps",
        "Host hint maximum",
        POSITIVE_U32,
    )
    .unit("kbps"),
    field(
        Audience::Host,
        Section::Network,
        "Hosting",
        "peers.host_on_launch",
        "Start hosting when the app opens",
        Kind::Flag,
    ),
    field(
        Audience::Host,
        Section::Network,
        "Peers",
        "peers.host_name",
        "Host name",
        Kind::Text,
    )
    .optional(),
    field(
        Audience::Client,
        Section::Network,
        "Peers",
        "peers.client_name",
        "Client name",
        Kind::Text,
    )
    .optional(),
    field(
        Audience::Host,
        Section::Network,
        "Peers",
        "peers.host_file",
        "Paired clients file",
        Kind::Text,
    )
    .optional(),
    field(
        Audience::Client,
        Section::Network,
        "Peers",
        "peers.client_file",
        "Paired hosts file",
        Kind::Text,
    )
    .optional(),
    field(
        Audience::Host,
        Section::Network,
        "Peers",
        "peers.write_pin_file",
        "Write the pairing PIN to a file",
        Kind::Flag,
    ),
    field(
        Audience::Host,
        Section::Network,
        "Peers",
        "peers.pin_file",
        "Pairing PIN file",
        Kind::Text,
    )
    .optional(),
    field(
        Audience::Host,
        Section::Network,
        "Peers",
        "peers.control_socket",
        "Daemon control socket",
        Kind::Flag,
    ),
    field(
        Audience::Host,
        Section::Network,
        "Peers",
        "peers.control_socket_path",
        "Control socket path",
        Kind::Text,
    )
    .optional(),
    field(
        Audience::Shared,
        Section::Input,
        "Forwarding",
        "input.keyboard",
        "Keyboard",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Input,
        "Forwarding",
        "input.pointer",
        "Pointer",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Input,
        "Forwarding",
        "input.touch",
        "Touch",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Input,
        "Forwarding",
        "input.gamepads",
        "Gamepads",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Input,
        "Forwarding",
        "input.max_gamepads",
        "Max gamepads",
        U8,
    ),
    field(
        Audience::Host,
        Section::Input,
        "Forwarding",
        "input.gamepad_personality",
        "Host gamepad type",
        Kind::Choice(GAMEPAD_PERSONALITIES),
    ),
    field(
        Audience::Client,
        Section::Input,
        "Forwarding",
        "input.pointer_mode",
        "Pointer mode",
        Kind::Choice(POINTER_MODES),
    ),
    field(
        Audience::Client,
        Section::Input,
        "Forwarding",
        "input.pointer_auto_delay_ms",
        "Auto pointer mode switch delay",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Client,
        Section::Input,
        "Forwarding",
        "input.release_chord",
        "Release chord",
        Kind::Choice(RELEASE_CHORDS),
    ),
    field(
        Audience::Client,
        Section::Input,
        "Forwarding",
        "input.inhibit_shortcuts",
        "Keep the desktop's shortcuts from the stream while engaged",
        Kind::Flag,
    ),
    field(
        Audience::Host,
        Section::Input,
        "Forwarding",
        "input.sync_locks",
        "Keep Caps Lock and Num Lock in step with the client",
        Kind::Flag,
    ),
    field(
        Audience::Client,
        Section::Input,
        "Hotkeys",
        "input.hotkeys",
        "Client hotkeys while engaged",
        Kind::Flag,
    ),
    field(
        Audience::Client,
        Section::Input,
        "Hotkeys",
        "input.hotkey_chord",
        "Hotkey modifiers",
        Kind::Choice(RELEASE_CHORDS),
    ),
    field(
        Audience::Client,
        Section::Input,
        "Hotkeys",
        "input.hotkey_fullscreen",
        "Fullscreen key",
        Kind::Choice(HOTKEY_KEYS),
    ),
    field(
        Audience::Client,
        Section::Input,
        "Hotkeys",
        "input.hotkey_stats",
        "Statistics key",
        Kind::Choice(HOTKEY_KEYS),
    ),
    field(
        Audience::Client,
        Section::Input,
        "Hotkeys",
        "input.hotkey_disconnect",
        "Disconnect key",
        Kind::Choice(HOTKEY_KEYS),
    ),
    field(
        Audience::Client,
        Section::Input,
        "Hotkeys",
        "input.hotkey_pointer_mode",
        "Pointer mode key",
        Kind::Choice(HOTKEY_KEYS),
    ),
    field(
        Audience::Client,
        Section::Input,
        "Hotkeys",
        "input.escape_hold_ms",
        "Hold Esc to leave",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Client,
        Section::Input,
        "Hotkeys",
        "input.fullscreen_hint_ms",
        "Fullscreen hint",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Client,
        Section::Input,
        "Tuning",
        "input.pointer_sensitivity",
        "Pointer sensitivity",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Input,
        "Tuning",
        "input.gamepad_deadzone",
        "Gamepad deadzone",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Input,
        "Rumble",
        "input.rumble_max_ms",
        "Longest rumble (0 = off)",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Client,
        Section::Input,
        "Tuning",
        "input.resend_delay_ms",
        "Resend delay",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Client,
        Section::Input,
        "Tuning",
        "input.snapshot_interval_ms",
        "Held-input snapshot interval (0 = off)",
        U16,
    )
    .unit("ms"),
    field(
        Audience::Host,
        Section::Input,
        "Forwarding",
        "input.accept_remote",
        "Inject the client's input on the host",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Input,
        "Clipboard",
        "input.clipboard",
        "Share the text clipboard",
        Kind::Choice(CLIPBOARD_DIRECTIONS),
    ),
    field(
        Audience::Shared,
        Section::Input,
        "Clipboard",
        "input.clipboard_max_bytes",
        "Largest clipboard text shared",
        CLIPBOARD_BYTES,
    )
    .unit("bytes"),
    field(
        Audience::Host,
        Section::Capture,
        "Source",
        "capture.source",
        "Source",
        Kind::Choice(SOURCE_KINDS),
    ),
    field(
        Audience::Host,
        Section::Capture,
        "Source",
        "capture.cursor",
        "Cursor",
        Kind::Choice(CURSOR_MODES),
    ),
    field(
        Audience::Host,
        Section::Capture,
        "Source",
        "capture.prefer_dmabuf",
        "Prefer DMA-BUF",
        Kind::Flag,
    ),
    field(
        Audience::Host,
        Section::Capture,
        "Source",
        "capture.dmabuf_gpu_fence_wait",
        "Wait for DMA-BUF fences on the GPU",
        Kind::Flag,
    ),
    field(
        Audience::Host,
        Section::Capture,
        "Source",
        "capture.dmabuf_fence_timeout_ms",
        "DMA-BUF fence timeout",
        U32,
    ),
    field(
        Audience::Host,
        Section::Capture,
        "Virtual output",
        "capture.virtual_max_resolution",
        "Largest virtual output",
        Kind::Resolution,
    ),
    field(
        Audience::Host,
        Section::Capture,
        "Virtual output",
        "capture.virtual_max_refresh_hz",
        "Fastest virtual output",
        U32,
    )
    .unit("Hz"),
    field(
        Audience::Host,
        Section::Capture,
        "Virtual output",
        "capture.virtual_min_resolution",
        "Smallest virtual output",
        Kind::Resolution,
    ),
    field(
        Audience::Host,
        Section::Capture,
        "Virtual output",
        "capture.virtual_resize_min_interval_ms",
        "Shortest time between resizes",
        Kind::Integer {
            min: 10,
            max: 60_000,
        },
    )
    .unit("ms"),
    field(
        Audience::Host,
        Section::Capture,
        "Virtual output",
        "capture.virtual_first_frame_timeout_ms",
        "Wait for the first frame at the offered size",
        Kind::Integer {
            min: 0,
            max: 10_000,
        },
    )
    .unit("ms"),
    field(
        Audience::Host,
        Section::Capture,
        "Session",
        "capture.persist_session",
        "Persist session",
        Kind::Flag,
    ),
    field(
        Audience::Host,
        Section::Capture,
        "Session",
        "capture.restore_token",
        "Restore token",
        Kind::Text,
    )
    .optional(),
    field(
        Audience::Host,
        Section::Capture,
        "Session",
        "capture.restore_token_file",
        "Restore token file",
        Kind::Text,
    )
    .optional(),
    field(
        Audience::Shared,
        Section::Scaling,
        "Chroma",
        "scaling.chroma_siting",
        "Chroma siting",
        Kind::Choice(CHROMA_SITINGS),
    ),
    field(
        Audience::Host,
        Section::Scaling,
        "Downscale",
        "scaling.downscale.filter",
        "Filter",
        Kind::Choice(SCALE_FILTERS),
    ),
    field(
        Audience::Host,
        Section::Scaling,
        "Downscale",
        "scaling.downscale.chroma_filter",
        "Chroma filter",
        Kind::Choice(SCALE_FILTERS),
    ),
    field(
        Audience::Host,
        Section::Scaling,
        "Downscale",
        "scaling.downscale.linear_light",
        "Linear light",
        Kind::Flag,
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Upscale",
        "scaling.upscale.filter",
        "Filter",
        Kind::Choice(SCALE_FILTERS),
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Upscale",
        "scaling.upscale.chroma_filter",
        "Chroma filter",
        Kind::Choice(SCALE_FILTERS),
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Upscale",
        "scaling.upscale.sigmoid",
        "Sigmoid",
        Kind::Flag,
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Upscale",
        "scaling.upscale.sigmoid_center",
        "Sigmoid center",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Upscale",
        "scaling.upscale.sigmoid_slope",
        "Sigmoid slope",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Upscale",
        "scaling.upscale.antiringing",
        "Anti-ringing",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Upscale",
        "scaling.upscale.chroma_antiringing",
        "Chroma anti-ringing",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Debanding",
        "scaling.deband.enabled",
        "Deband",
        Kind::Flag,
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Debanding",
        "scaling.deband.iterations",
        "Iterations",
        Kind::Integer { min: 1, max: 4 },
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Debanding",
        "scaling.deband.radius",
        "Radius",
        Kind::Integer { min: 1, max: 64 },
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Debanding",
        "scaling.deband.threshold",
        "Threshold",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Debanding",
        "scaling.deband.chroma_threshold",
        "Chroma threshold",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Scaling,
        "Debanding",
        "scaling.deband.limit",
        "Largest change",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Color,
        "Display target",
        "color.target.primaries",
        "Primaries",
        Kind::Primaries,
    )
    .optional(),
    field(
        Audience::Client,
        Section::Color,
        "Display target",
        "color.target.transfer",
        "Transfer function",
        Kind::Choice(TRANSFERS),
    )
    .optional(),
    field(
        Audience::Client,
        Section::Color,
        "Display target",
        "color.target.reference_white_nits",
        "Reference white",
        HDR_NITS,
    )
    .unit("nits")
    .optional(),
    field(
        Audience::Client,
        Section::Color,
        "Display target",
        "color.target.peak_nits",
        "Peak luminance",
        HDR_NITS,
    )
    .unit("nits")
    .optional(),
    field(
        Audience::Client,
        Section::Color,
        "Display target",
        "color.target.black_nits",
        "Black level",
        Kind::Decimal,
    )
    .unit("nits")
    .optional(),
    field(
        Audience::Shared,
        Section::Color,
        "Tone mapping",
        "color.tone_mapping.operator",
        "Operator",
        Kind::Choice(TONE_OPERATORS),
    ),
    field(
        Audience::Shared,
        Section::Color,
        "Tone mapping",
        "color.tone_mapping.dynamic_peak",
        "Dynamic peak detection",
        Kind::Flag,
    ),
    field(
        Audience::Shared,
        Section::Color,
        "Tone mapping",
        "color.tone_mapping.peak_percentile",
        "Peak percentile",
        Kind::Decimal,
    )
    .unit("%"),
    field(
        Audience::Shared,
        Section::Color,
        "Tone mapping",
        "color.tone_mapping.smoothing_frames",
        "Peak smoothing",
        POSITIVE_U32,
    )
    .unit("frames"),
    field(
        Audience::Shared,
        Section::Color,
        "Tone mapping",
        "color.tone_mapping.scene_threshold_db",
        "Scene cut threshold",
        Kind::Decimal,
    )
    .unit("dB"),
    field(
        Audience::Shared,
        Section::Color,
        "Gamut mapping",
        "color.gamut_mapping.mode",
        "Mode",
        Kind::Choice(GAMUT_MODES),
    ),
    field(
        Audience::Shared,
        Section::Color,
        "Gamut mapping",
        "color.gamut_mapping.knee",
        "Compression knee",
        Kind::Decimal,
    ),
    field(
        Audience::Client,
        Section::Color,
        "Processing",
        "color.processing.method",
        "Method",
        Kind::Choice(COLOR_METHODS),
    ),
    field(
        Audience::Client,
        Section::Color,
        "Processing",
        "color.processing.lut_size",
        "LUT size",
        LUT_SIZES,
    )
    .unit("points per axis"),
    field(
        Audience::Client,
        Section::Color,
        "Output",
        "color.output.color_space",
        "Surface colour space",
        Kind::Choice(OUTPUT_COLOR_SPACES),
    ),
    field(
        Audience::Client,
        Section::Color,
        "Output",
        "color.output.format",
        "Swapchain format",
        Kind::Choice(OUTPUT_FORMATS),
    ),
    field(
        Audience::Client,
        Section::Color,
        "Output",
        "color.output.dither",
        "Dithering",
        Kind::Choice(DITHER_MODES),
    ),
    field(
        Audience::Client,
        Section::Color,
        "Output",
        "color.output.query_display",
        "Ask the compositor for the display",
        Kind::Flag,
    ),
];
