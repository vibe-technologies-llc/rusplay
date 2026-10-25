#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Video,
    Audio,
    Network,
    Input,
    Capture,
    Scaling,
}

impl Section {
    pub const ALL: [Self; 6] = [
        Self::Video,
        Self::Audio,
        Self::Network,
        Self::Input,
        Self::Capture,
        Self::Scaling,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Network => "Network",
            Self::Input => "Input",
            Self::Capture => "Capture",
            Self::Scaling => "Scaling",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Flag,
    Integer { min: i64, max: i64 },
    Decimal,
    Choice(&'static [&'static str]),
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

const CODECS: &[&str] = &["h264", "hevc", "av1"];
const CHROMA: &[&str] = &["4:2:0", "4:2:2", "4:4:4"];
const BIT_DEPTHS: &[&str] = &["eight", "ten", "twelve"];
const PRIMARIES: &[&str] = &["bt709", "bt2020"];
const TRANSFERS: &[&str] = &["bt709", "srgb", "pq", "hlg"];
const RANGES: &[&str] = &["limited", "full"];
const BACKENDS: &[&str] = &["vaapi", "nvenc", "x264", "x265", "svt-av1"];
const DECODER_BACKENDS: &[&str] = &["vaapi", "nvdec", "software"];
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
const POINTER_MODES: &[&str] = &["absolute", "relative"];
const SOURCE_KINDS: &[&str] = &["monitor", "window", "any"];
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
    any_of: &["vaapi", "nvenc"],
}];
const VARIABLE_BITRATE: &[Condition] = &[
    Condition {
        path: BACKEND_PATH,
        any_of: &["vaapi", "nvenc"],
    },
    Condition {
        path: MODE_PATH,
        any_of: &["vbr"],
    },
];
const CONSTANT_QUALITY: &[Condition] = &[
    Condition {
        path: BACKEND_PATH,
        any_of: &["vaapi", "nvenc"],
    },
    Condition {
        path: MODE_PATH,
        any_of: &["constant-quality"],
    },
];

const fn field(
    section: Section,
    group: &'static str,
    path: &'static str,
    label: &'static str,
    kind: Kind,
) -> Field {
    Field {
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

pub static FIELDS: &[Field] = &[
    field(
        Section::Video,
        "Stream",
        "video.resolution",
        "Resolution",
        Kind::Resolution,
    )
    .optional(),
    field(
        Section::Video,
        "Stream",
        "video.frame_rate",
        "Frame rate",
        Kind::FrameRate,
    )
    .unit("fps"),
    field(
        Section::Video,
        "Stream",
        "video.codec",
        "Codec",
        Kind::Choice(CODECS),
    ),
    field(
        Section::Video,
        "Stream",
        "video.bitrate_kbps",
        "Bitrate",
        U32,
    )
    .unit("kbps"),
    field(
        Section::Video,
        "Pixel format",
        "video.pixel_format.chroma",
        "Chroma subsampling",
        Kind::Choice(CHROMA),
    ),
    field(
        Section::Video,
        "Pixel format",
        "video.pixel_format.depth",
        "Bit depth",
        Kind::Choice(BIT_DEPTHS),
    ),
    field(
        Section::Video,
        "Colour",
        "video.color.primaries",
        "Primaries",
        Kind::Choice(PRIMARIES),
    ),
    field(
        Section::Video,
        "Colour",
        "video.color.transfer",
        "Transfer function",
        Kind::Choice(TRANSFERS),
    ),
    field(
        Section::Video,
        "Colour",
        "video.color.range",
        "Range",
        Kind::Choice(RANGES),
    ),
    field(
        Section::Video,
        "Colour",
        "video.color.hdr.reference_white_nits",
        "HDR reference white",
        HDR_NITS,
    )
    .unit("nits"),
    field(
        Section::Video,
        "Colour",
        "video.color.hdr.peak_nits",
        "HDR peak luminance",
        HDR_NITS,
    )
    .unit("nits"),
    field(
        Section::Video,
        "Encoder",
        BACKEND_PATH,
        "Backend",
        Kind::Choice(BACKENDS),
    ),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.device",
        "Device",
        Kind::Text,
    )
    .when(VAAPI),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.low_power",
        "Low power mode",
        Kind::Flag,
    )
    .when(VAAPI),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.gpu_index",
        "GPU index",
        U32,
    )
    .when(NVENC),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.preset",
        "Preset",
        Kind::Choice(NVENC_PRESETS),
    )
    .when(NVENC),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.tuning",
        "Tuning",
        Kind::Choice(NVENC_TUNINGS),
    )
    .when(NVENC),
    field(
        Section::Video,
        "Encoder",
        MODE_PATH,
        "Rate control",
        Kind::Choice(RATE_CONTROL_MODES),
    )
    .when(HARDWARE_RATE_CONTROL),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.rate_control.max_bitrate_kbps",
        "Maximum bitrate",
        U32,
    )
    .unit("kbps")
    .when(VARIABLE_BITRATE),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.rate_control.qp",
        "Quantizer",
        U8,
    )
    .when(CONSTANT_QUALITY),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.preset",
        "Preset",
        Kind::Choice(SOFTWARE_PRESETS),
    )
    .when(SOFTWARE_X26X),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.tune",
        "Tune",
        Kind::MultiChoice(X264_TUNES),
    )
    .when(X264),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.tune",
        "Tune",
        Kind::Choice(X265_TUNES),
    )
    .optional()
    .when(X265),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.preset",
        "Preset",
        U8,
    )
    .when(SVT_AV1),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.low_latency",
        "Low latency",
        Kind::Flag,
    )
    .when(SVT_AV1),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.log_level",
        "Log level",
        Kind::Choice(SVT_AV1_LOG_LEVELS),
    )
    .when(SVT_AV1),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.threads",
        "Threads",
        POSITIVE_U32,
    )
    .optional()
    .when(SOFTWARE_THREADED),
    field(
        Section::Video,
        "Encoder",
        "video.encoder.params",
        "Parameters",
        Kind::Pairs,
    ),
    field(
        Section::Video,
        "Decoder",
        DECODER_BACKEND_PATH,
        "Backend",
        Kind::Choice(DECODER_BACKENDS),
    ),
    field(
        Section::Video,
        "Decoder",
        "video.decoder.device",
        "Device",
        Kind::Text,
    )
    .when(VAAPI_DECODER),
    field(
        Section::Video,
        "Decoder",
        "video.decoder.gpu_index",
        "GPU index",
        U32,
    )
    .when(NVDEC_DECODER),
    field(
        Section::Video,
        "Decoder",
        "video.decoder.threads",
        "Threads",
        POSITIVE_U32,
    )
    .optional()
    .when(SOFTWARE_DECODER),
    field(
        Section::Audio,
        "Stream",
        "audio.codec",
        "Codec",
        Kind::Choice(AUDIO_CODECS),
    ),
    field(
        Section::Audio,
        "Stream",
        "audio.sample_rate",
        "Sample rate",
        Kind::Choice(SAMPLE_RATES),
    )
    .unit("Hz"),
    field(
        Section::Audio,
        "Stream",
        "audio.channels",
        "Channels",
        Kind::Choice(CHANNEL_LAYOUTS),
    ),
    field(
        Section::Audio,
        "Stream",
        "audio.jitter_buffer_ms",
        "Jitter buffer",
        U32,
    )
    .unit("ms"),
    field(
        Section::Audio,
        "Opus",
        "audio.opus.bitrate_kbps",
        "Bitrate",
        U32,
    )
    .unit("kbps"),
    field(
        Section::Audio,
        "Opus",
        "audio.opus.frame_duration",
        "Frame duration",
        Kind::Choice(OPUS_FRAME_DURATIONS),
    ),
    field(
        Section::Audio,
        "Opus",
        "audio.opus.inband_fec",
        "In-band FEC",
        Kind::Flag,
    ),
    field(
        Section::Audio,
        "FLAC",
        "audio.flac.compression_level",
        "Compression level",
        U8,
    ),
    field(
        Section::Audio,
        "FLAC",
        "audio.flac.block_size",
        "Block size",
        U16,
    )
    .unit("samples"),
    field(
        Section::Network,
        "Socket",
        "network.bind_address",
        "Bind address",
        Kind::Address,
    ),
    field(Section::Network, "Socket", "network.port", "Port", U16),
    field(
        Section::Network,
        "Socket",
        "network.socket_buffer_bytes",
        "Socket buffer",
        U32,
    )
    .unit("bytes"),
    field(Section::Network, "Transport", "network.mtu", "MTU", U16).unit("bytes"),
    field(
        Section::Network,
        "Transport",
        "network.fec_overhead_percent",
        "FEC overhead",
        U8,
    )
    .unit("%"),
    field(
        Section::Network,
        "Transport",
        "network.pacing",
        "Pacing",
        Kind::Flag,
    ),
    field(
        Section::Network,
        "Transport",
        "network.jitter_buffer_ms",
        "Jitter buffer",
        U16,
    )
    .unit("ms"),
    field(
        Section::Network,
        "Transport",
        "network.reassembly_timeout_ms",
        "Reassembly timeout",
        U16,
    )
    .unit("ms"),
    field(
        Section::Network,
        "Control",
        "network.control_retransmit_ms",
        "Retransmit interval",
        U16,
    )
    .unit("ms"),
    field(
        Section::Network,
        "Control",
        "network.control_max_retransmits",
        "Max retransmits",
        U8,
    ),
    field(
        Section::Network,
        "Discovery",
        "network.discovery_port",
        "Discovery port",
        U16,
    ),
    field(
        Section::Network,
        "Discovery",
        "network.discovery_group",
        "Multicast group",
        Kind::Address,
    ),
    field(
        Section::Network,
        "Pairing",
        "network.pairing_pin_digits",
        "PIN digits",
        U8,
    ),
    field(
        Section::Network,
        "Pairing",
        "network.pairing_max_attempts",
        "Max attempts",
        U8,
    ),
    field(
        Section::Network,
        "Pairing",
        "network.pairing_timeout_ms",
        "Pairing timeout",
        U32,
    )
    .unit("ms"),
    field(
        Section::Network,
        "Pairing",
        "network.reconnect_timeout_ms",
        "Reconnect timeout",
        U32,
    )
    .unit("ms"),
    field(
        Section::Network,
        "Peers",
        "peers.host_file",
        "Paired clients file",
        Kind::Text,
    )
    .optional(),
    field(
        Section::Network,
        "Peers",
        "peers.client_file",
        "Paired hosts file",
        Kind::Text,
    )
    .optional(),
    field(
        Section::Input,
        "Forwarding",
        "input.keyboard",
        "Keyboard",
        Kind::Flag,
    ),
    field(
        Section::Input,
        "Forwarding",
        "input.pointer",
        "Pointer",
        Kind::Flag,
    ),
    field(
        Section::Input,
        "Forwarding",
        "input.gamepads",
        "Gamepads",
        Kind::Flag,
    ),
    field(
        Section::Input,
        "Forwarding",
        "input.max_gamepads",
        "Max gamepads",
        U8,
    ),
    field(
        Section::Input,
        "Forwarding",
        "input.pointer_mode",
        "Pointer mode",
        Kind::Choice(POINTER_MODES),
    ),
    field(
        Section::Capture,
        "Source",
        "capture.source",
        "Source",
        Kind::Choice(SOURCE_KINDS),
    ),
    field(
        Section::Capture,
        "Source",
        "capture.cursor",
        "Cursor",
        Kind::Choice(CURSOR_MODES),
    ),
    field(
        Section::Capture,
        "Source",
        "capture.prefer_dmabuf",
        "Prefer DMA-BUF",
        Kind::Flag,
    ),
    field(
        Section::Capture,
        "Session",
        "capture.persist_session",
        "Persist session",
        Kind::Flag,
    ),
    field(
        Section::Capture,
        "Session",
        "capture.restore_token",
        "Restore token",
        Kind::Text,
    )
    .optional(),
    field(
        Section::Capture,
        "Session",
        "capture.restore_token_file",
        "Restore token file",
        Kind::Text,
    )
    .optional(),
    field(
        Section::Scaling,
        "Downscale",
        "scaling.downscale.filter",
        "Filter",
        Kind::Choice(SCALE_FILTERS),
    ),
    field(
        Section::Scaling,
        "Downscale",
        "scaling.downscale.chroma_filter",
        "Chroma filter",
        Kind::Choice(SCALE_FILTERS),
    ),
    field(
        Section::Scaling,
        "Downscale",
        "scaling.downscale.linear_light",
        "Linear light",
        Kind::Flag,
    ),
    field(
        Section::Scaling,
        "Upscale",
        "scaling.upscale.filter",
        "Filter",
        Kind::Choice(SCALE_FILTERS),
    ),
    field(
        Section::Scaling,
        "Upscale",
        "scaling.upscale.chroma_filter",
        "Chroma filter",
        Kind::Choice(SCALE_FILTERS),
    ),
    field(
        Section::Scaling,
        "Upscale",
        "scaling.upscale.sigmoid",
        "Sigmoid",
        Kind::Flag,
    ),
    field(
        Section::Scaling,
        "Upscale",
        "scaling.upscale.sigmoid_center",
        "Sigmoid center",
        Kind::Decimal,
    ),
    field(
        Section::Scaling,
        "Upscale",
        "scaling.upscale.sigmoid_slope",
        "Sigmoid slope",
        Kind::Decimal,
    ),
    field(
        Section::Scaling,
        "Upscale",
        "scaling.upscale.antiringing",
        "Anti-ringing",
        Kind::Decimal,
    ),
];
