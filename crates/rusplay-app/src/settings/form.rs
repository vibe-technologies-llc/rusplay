use std::net::IpAddr;

use rusplay_config::{
    Config, ConfigError, ConfigRole, NvdecDecoderConfig, NvencConfig, RateControl,
    SoftwareDecoderConfig, SvtAv1Config, VaapiConfig, VaapiDecoderConfig, VideoDecoderConfig,
    VideoEncoderConfig, X264Config, X265Config,
};
use thiserror::Error;
use toml::{Table, Value as Toml};

use crate::settings::{
    primaries,
    schema::{Condition, FIELDS, Field, Kind, Section, audience},
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pair {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Flag(bool),
    Text(String),
    Set(Vec<String>),
    Pairs(Vec<Pair>),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FieldError {
    #[error("a value is required")]
    Required,
    #[error("not a whole number")]
    NotAnInteger,
    #[error("must be between {min} and {max}")]
    OutOfRange { min: i64, max: i64 },
    #[error("not a number")]
    NotANumber,
    #[error("not an IP address")]
    NotAnAddress,
    #[error("expected WIDTHxHEIGHT")]
    NotAResolution,
    #[error("expected FPS or NUMERATOR/DENOMINATOR")]
    NotAFrameRate,
    #[error("parameter names cannot be empty")]
    EmptyParameterName,
    #[error("parameter {0} is listed twice")]
    DuplicateParameter(String),
    #[error("{0} is not one of the options")]
    UnknownChoice(String),
    #[error(
        "expected one name, or custom followed by the red, green, blue and white points as X,Y"
    )]
    NotPrimaries,
    #[error("these points do not describe a usable gamut")]
    PrimariesRejected,
    #[error("the value does not fit this setting")]
    WrongShape,
}

#[derive(Debug, Error)]
pub enum FormError {
    #[error("{label}: {error}")]
    Field {
        field: usize,
        label: &'static str,
        error: FieldError,
    },
    #[error("{error}")]
    Rejected {
        field: Option<usize>,
        error: ConfigError,
    },
    #[error("assembling the configuration failed: {0}")]
    Assemble(#[from] toml::de::Error),
}

impl FormError {
    pub const fn field(&self) -> Option<usize> {
        match self {
            Self::Field { field, .. } => Some(*field),
            Self::Rejected { field, .. } => *field,
            Self::Assemble(_) => None,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Field { error, .. } => error.to_string(),
            other => other.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormModel {
    values: Vec<Value>,
    role: ConfigRole,
}

impl FormModel {
    pub fn from_config(config: &Config, role: ConfigRole) -> Result<Self, ConfigError> {
        let mut sources = vec![to_table(config)?];
        for encoder in encoder_alternatives() {
            let mut alternative = config.clone();
            alternative.video.encoder = encoder;
            sources.push(to_table(&alternative)?);
        }
        for decoder in decoder_alternatives() {
            let mut alternative = config.clone();
            alternative.video.decoder = decoder;
            sources.push(to_table(&alternative)?);
        }

        let values = FIELDS
            .iter()
            .map(|field| {
                let source = sources
                    .iter()
                    .find(|table| conditions_hold(table, field.when))
                    .unwrap_or(&sources[0]);
                read_value(source, field)
            })
            .collect();

        Ok(Self { values, role })
    }

    pub fn value(&self, field: usize) -> &Value {
        &self.values[field]
    }

    pub fn text(&self, field: usize) -> &str {
        match &self.values[field] {
            Value::Text(text) => text,
            Value::Flag(_) | Value::Set(_) | Value::Pairs(_) => "",
        }
    }

    pub fn set_text(&mut self, field: usize, text: impl Into<String>) {
        self.values[field] = Value::Text(text.into());
    }

    pub fn set_flag(&mut self, field: usize, flag: bool) {
        self.values[field] = Value::Flag(flag);
    }

    pub fn toggle_member(&mut self, field: usize, member: &str) {
        if let Value::Set(members) = &mut self.values[field] {
            match members.iter().position(|existing| existing == member) {
                Some(index) => {
                    members.remove(index);
                }
                None => members.push(member.to_owned()),
            }
        }
    }

    pub fn pairs(&self, field: usize) -> &[Pair] {
        match &self.values[field] {
            Value::Pairs(pairs) => pairs,
            Value::Flag(_) | Value::Text(_) | Value::Set(_) => &[],
        }
    }

    pub fn add_pair(&mut self, field: usize) {
        if let Value::Pairs(pairs) = &mut self.values[field] {
            pairs.push(Pair::default());
        }
    }

    pub fn remove_pair(&mut self, field: usize, index: usize) {
        if let Value::Pairs(pairs) = &mut self.values[field]
            && index < pairs.len()
        {
            pairs.remove(index);
        }
    }

    pub fn set_pair(&mut self, field: usize, index: usize, pair: Pair) {
        if let Value::Pairs(pairs) = &mut self.values[field]
            && let Some(slot) = pairs.get_mut(index)
        {
            *slot = pair;
        }
    }

    pub fn is_visible(&self, field: usize) -> bool {
        audience(FIELDS[field].path).allows(self.role == ConfigRole::Host)
            && self.conditions_hold(field)
    }

    fn conditions_hold(&self, field: usize) -> bool {
        FIELDS[field].when.iter().all(|condition| {
            self.selector(condition.path)
                .is_some_and(|value| condition.any_of.contains(&value))
        })
    }

    fn selector(&self, path: &str) -> Option<&str> {
        (0..FIELDS.len())
            .find(|&index| {
                FIELDS[index].path == path
                    && audience(FIELDS[index].path).allows(self.role == ConfigRole::Host)
                    && self.conditions_hold(index)
            })
            .map(|index| self.text(index))
    }

    pub fn visible_fields(&self, section: Section) -> Vec<usize> {
        (0..FIELDS.len())
            .filter(|&index| FIELDS[index].section == section && self.is_visible(index))
            .collect()
    }

    pub fn visible_field_with_path(&self, path: &str) -> Option<usize> {
        (0..FIELDS.len()).find(|&index| FIELDS[index].path == path && self.is_visible(index))
    }

    pub fn check_field(&self, field: usize) -> Result<(), FieldError> {
        self.field_value(field).map(|_| ())
    }

    pub fn build(&self) -> Result<Config, Vec<FormError>> {
        let mut table = Table::new();
        let mut issues = Vec::new();

        for index in (0..FIELDS.len()).filter(|&index| self.is_visible(index)) {
            match self.field_value(index) {
                Ok(Some(value)) => insert(&mut table, FIELDS[index].path, value),
                Ok(None) => {}
                Err(error) => issues.push(FormError::Field {
                    field: index,
                    label: FIELDS[index].label,
                    error,
                }),
            }
        }

        if !issues.is_empty() {
            return Err(issues);
        }

        let config: Config = Toml::Table(table)
            .try_into()
            .map_err(|error| vec![FormError::Assemble(error)])?;

        config.validate().map_err(|error| {
            vec![FormError::Rejected {
                field: error
                    .field()
                    .and_then(|path| self.visible_field_with_path(path)),
                error,
            }]
        })?;

        Ok(config)
    }

    fn field_value(&self, index: usize) -> Result<Option<Toml>, FieldError> {
        let field = &FIELDS[index];

        match (&self.values[index], field.kind) {
            (Value::Flag(flag), Kind::Flag) => Ok(Some(Toml::Boolean(*flag))),
            (Value::Set(members), Kind::MultiChoice(options)) => {
                members
                    .iter()
                    .try_for_each(|member| expect_choice(options, member))?;
                Ok(Some(Toml::Array(
                    members.iter().cloned().map(Toml::String).collect(),
                )))
            }
            (Value::Pairs(pairs), Kind::Pairs) => pairs_value(pairs).map(Some),
            (Value::Text(text), kind) => text_value(text.trim(), kind, field.optional),
            _ => Err(FieldError::WrongShape),
        }
    }
}

fn to_table(config: &Config) -> Result<Table, ConfigError> {
    Table::try_from(config).map_err(|source| ConfigError::Serialize { source })
}

fn decoder_alternatives() -> [VideoDecoderConfig; 3] {
    [
        VideoDecoderConfig::Vaapi(VaapiDecoderConfig::default()),
        VideoDecoderConfig::Nvdec(NvdecDecoderConfig::default()),
        VideoDecoderConfig::Software(SoftwareDecoderConfig::default()),
    ]
}

fn encoder_alternatives() -> Vec<VideoEncoderConfig> {
    let rate_controls = [
        RateControl::Cbr,
        RateControl::DEFAULT_VBR,
        RateControl::DEFAULT_CONSTANT_QUALITY,
    ];

    let hardware = rate_controls.into_iter().flat_map(|rate_control| {
        [
            VideoEncoderConfig::Vaapi(VaapiConfig {
                rate_control,
                ..VaapiConfig::default()
            }),
            VideoEncoderConfig::Nvenc(NvencConfig {
                rate_control,
                ..NvencConfig::default()
            }),
        ]
    });

    hardware
        .chain([
            VideoEncoderConfig::X264(X264Config::default()),
            VideoEncoderConfig::X265(X265Config::default()),
            VideoEncoderConfig::SvtAv1(SvtAv1Config::default()),
        ])
        .collect()
}

fn lookup<'a>(table: &'a Table, path: &str) -> Option<&'a Toml> {
    let mut segments = path.split('.');
    let mut current = table.get(segments.next()?)?;

    for segment in segments {
        current = current.as_table()?.get(segment)?;
    }

    Some(current)
}

fn conditions_hold(table: &Table, conditions: &[Condition]) -> bool {
    conditions.iter().all(|condition| {
        lookup(table, condition.path)
            .and_then(Toml::as_str)
            .is_some_and(|value| condition.any_of.contains(&value))
    })
}

fn insert(table: &mut Table, path: &str, value: Toml) {
    match path.split_once('.') {
        Some((head, rest)) => {
            let child = table
                .entry(head.to_owned())
                .or_insert_with(|| Toml::Table(Table::new()));
            if let Toml::Table(child) = child {
                insert(child, rest, value);
            }
        }
        None => {
            table.insert(path.to_owned(), value);
        }
    }
}

fn read_value(table: &Table, field: &Field) -> Value {
    let found = lookup(table, field.path);

    match field.kind {
        Kind::Flag => Value::Flag(found.and_then(Toml::as_bool).unwrap_or_default()),
        Kind::MultiChoice(_) => {
            Value::Set(found.and_then(Toml::as_array).map_or_default(|items| {
                items
                    .iter()
                    .filter_map(Toml::as_str)
                    .map(str::to_owned)
                    .collect()
            }))
        }
        Kind::Pairs => Value::Pairs(found.and_then(Toml::as_table).map_or_default(|entries| {
            entries
                .iter()
                .map(|(key, value)| Pair {
                    key: key.clone(),
                    value: value.as_str().unwrap_or_default().to_owned(),
                })
                .collect()
        })),
        Kind::Integer { .. } | Kind::Decimal => Value::Text(found.map_or_default(number_text)),
        Kind::Choice(_) | Kind::Text | Kind::Address => {
            Value::Text(found.and_then(Toml::as_str).unwrap_or_default().to_owned())
        }
        Kind::Resolution => Value::Text(found.map_or_default(resolution_text)),
        Kind::FrameRate => Value::Text(found.map_or_default(frame_rate_text)),
        Kind::Primaries => Value::Text(found.map_or_default(primaries::text_of)),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn number_text(value: &Toml) -> String {
    match value {
        Toml::Integer(integer) => integer.to_string(),
        Toml::Float(float) => (*float as f32).to_string(),
        _ => String::new(),
    }
}

fn dimension(table: &Toml, key: &str) -> Option<i64> {
    table.as_table()?.get(key)?.as_integer()
}

fn resolution_text(value: &Toml) -> String {
    match (dimension(value, "width"), dimension(value, "height")) {
        (Some(width), Some(height)) => format!("{width}x{height}"),
        _ => String::new(),
    }
}

fn frame_rate_text(value: &Toml) -> String {
    match (
        dimension(value, "numerator"),
        dimension(value, "denominator"),
    ) {
        (Some(numerator), Some(1)) => numerator.to_string(),
        (Some(numerator), Some(denominator)) => format!("{numerator}/{denominator}"),
        _ => String::new(),
    }
}

fn expect_choice(options: &[&str], value: &str) -> Result<(), FieldError> {
    if options.contains(&value) {
        Ok(())
    } else {
        Err(FieldError::UnknownChoice(value.to_owned()))
    }
}

fn text_value(text: &str, kind: Kind, optional: bool) -> Result<Option<Toml>, FieldError> {
    if text.is_empty() {
        return if optional {
            Ok(None)
        } else {
            Err(FieldError::Required)
        };
    }

    let value = match kind {
        Kind::Integer { min, max } => integer_value(text, min, max)?,
        Kind::Decimal => text
            .parse::<f64>()
            .ok()
            .filter(|float| float.is_finite())
            .map(Toml::Float)
            .ok_or(FieldError::NotANumber)?,
        Kind::Choice(options) => {
            expect_choice(options, text)?;
            Toml::String(text.to_owned())
        }
        Kind::Text => Toml::String(text.to_owned()),
        Kind::Address => text
            .parse::<IpAddr>()
            .map(|address| Toml::String(address.to_string()))
            .map_err(|_| FieldError::NotAnAddress)?,
        Kind::Resolution => resolution_value(text)?,
        Kind::FrameRate => frame_rate_value(text)?,
        Kind::Primaries => primaries::value(text)?,
        Kind::Flag | Kind::MultiChoice(_) | Kind::Pairs => return Err(FieldError::WrongShape),
    };

    Ok(Some(value))
}

fn integer_value(text: &str, min: i64, max: i64) -> Result<Toml, FieldError> {
    let number = text.parse::<i64>().map_err(|_| FieldError::NotAnInteger)?;

    if (min..=max).contains(&number) {
        Ok(Toml::Integer(number))
    } else {
        Err(FieldError::OutOfRange { min, max })
    }
}

fn positive(text: &str) -> Option<i64> {
    text.trim()
        .parse::<u32>()
        .ok()
        .filter(|&number| number > 0)
        .map(i64::from)
}

fn resolution_value(text: &str) -> Result<Toml, FieldError> {
    let (width, height) = text
        .split_once(['x', 'X', '×'])
        .ok_or(FieldError::NotAResolution)?;
    let width = positive(width).ok_or(FieldError::NotAResolution)?;
    let height = positive(height).ok_or(FieldError::NotAResolution)?;

    Ok(Toml::Table(Table::from_iter([
        ("width".to_owned(), Toml::Integer(width)),
        ("height".to_owned(), Toml::Integer(height)),
    ])))
}

fn frame_rate_value(text: &str) -> Result<Toml, FieldError> {
    let (numerator, denominator) = match text.split_once('/') {
        Some((numerator, denominator)) => (positive(numerator), positive(denominator)),
        None => (positive(text), Some(1)),
    };
    let numerator = numerator.ok_or(FieldError::NotAFrameRate)?;
    let denominator = denominator.ok_or(FieldError::NotAFrameRate)?;

    Ok(Toml::Table(Table::from_iter([
        ("numerator".to_owned(), Toml::Integer(numerator)),
        ("denominator".to_owned(), Toml::Integer(denominator)),
    ])))
}

fn pairs_value(pairs: &[Pair]) -> Result<Toml, FieldError> {
    let mut table = Table::new();

    for pair in pairs
        .iter()
        .filter(|pair| !(pair.key.trim().is_empty() && pair.value.trim().is_empty()))
    {
        let key = pair.key.trim();
        if key.is_empty() {
            return Err(FieldError::EmptyParameterName);
        }

        let previous = table.insert(key.to_owned(), Toml::String(pair.value.trim().to_owned()));
        if previous.is_some() {
            return Err(FieldError::DuplicateParameter(key.to_owned()));
        }
    }

    Ok(Toml::Table(table))
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, num::NonZeroU32};

    use rusplay_config::{EncoderParams, NvencPreset, X264Tune, X265Tune};
    use rusplay_core::{
        Chromaticities, Chromaticity, ColorPrimaries, ContentLightLevel, FrameRate, Luminance,
        MasteringDisplay, MatrixCoefficients, Resolution, TransferFunction,
    };

    use super::*;
    use crate::settings::schema::{Audience, audience};

    fn form(config: &Config) -> FormModel {
        form_as(config, ConfigRole::Host)
    }

    fn client_form(config: &Config) -> FormModel {
        form_as(config, ConfigRole::Client)
    }

    fn form_as(config: &Config, role: ConfigRole) -> FormModel {
        FormModel::from_config(config, role).unwrap()
    }

    fn role_for(index: usize) -> ConfigRole {
        match audience(FIELDS[index].path) {
            Audience::Client => ConfigRole::Client,
            Audience::Host | Audience::Shared => ConfigRole::Host,
        }
    }

    fn index_of(path: &str, label: &str) -> usize {
        FIELDS
            .iter()
            .position(|field| field.path == path && field.label == label)
            .unwrap()
    }

    fn first_with_path(path: &str) -> usize {
        FIELDS.iter().position(|field| field.path == path).unwrap()
    }

    fn with_encoder(encoder: VideoEncoderConfig) -> Config {
        let mut config = Config::default();
        config.video.encoder = encoder;

        config
    }

    fn with_custom_primaries(base: &Config) -> Config {
        let mut custom = base.clone();
        let wide = ColorPrimaries::Custom(
            Chromaticities::new(
                Chromaticity::from_xy(0.708, 0.292).unwrap(),
                Chromaticity::from_xy(0.17, 0.797).unwrap(),
                Chromaticity::from_xy(0.131, 0.046).unwrap(),
                Chromaticity::D65,
            )
            .unwrap(),
        );
        custom.video.color.primaries = wide;
        custom.color.target.primaries = Some(wide);
        if let Some(mastering) = &mut custom.video.color.hdr.mastering {
            mastering.primaries = wide;
        }

        custom
    }

    fn populated_decoders() -> [VideoDecoderConfig; 3] {
        [
            VideoDecoderConfig::Vaapi(VaapiDecoderConfig {
                device: "/dev/dri/renderD129".into(),
                copy_output: true,
            }),
            VideoDecoderConfig::Nvdec(NvdecDecoderConfig { gpu_index: 1 }),
            VideoDecoderConfig::Software(SoftwareDecoderConfig {
                threads: NonZeroU32::new(4),
            }),
        ]
    }

    fn populated_configs() -> Vec<Config> {
        let mut base = Config::default();
        base.video.resolution = Some(Resolution::new(2560, 1440));
        base.video.frame_rate = FrameRate::new(60_000, 1001);
        base.video.bitrate_kbps = 123_456;
        base.scaling.upscale.antiringing = 0.35;
        base.capture.restore_token = Some("token".to_owned());
        base.capture.restore_token_file = Some("/tmp/rusplay-token".into());
        base.network.bind_address = "192.168.1.5".parse().unwrap();
        base.peers.host_file = Some("/tmp/rusplay-clients".into());
        base.peers.host_name = Some("living room".to_owned());
        base.peers.client_name = Some("desk".to_owned());
        base.peers.client_file = Some("/tmp/rusplay-hosts".into());
        base.peers.pin_file = Some("/tmp/rusplay-pin".into());
        base.peers.control_socket_path = Some("/tmp/rusplay-control".into());
        base.audio.flac.compression_level = 5;
        base.audio.opus.bitrate_kbps = 128;
        base.video.color.matrix = Some(MatrixCoefficients::Bt601);
        base.color.target.primaries = Some(ColorPrimaries::DisplayP3);
        base.color.target.transfer = Some(TransferFunction::Pq);
        base.color.target.reference_white_nits = Some(203);
        base.color.target.peak_nits = Some(600);
        base.color.target.black_nits = Some(Luminance::from_units(500).unwrap());
        base.color.gamut_mapping.knee = 0.75;
        base.color.tone_mapping.dynamic_peak = true;
        base.color.tone_mapping.peak_percentile = 99.9;
        base.video.color.hdr.mastering = Some(MasteringDisplay {
            primaries: ColorPrimaries::Bt2020,
            max_nits: Luminance::from_whole_nits(4000).unwrap(),
            min_nits: Luminance::from_units(50).unwrap(),
        });
        base.video.color.hdr.content_light = Some(ContentLightLevel {
            max_cll_nits: 1200,
            max_fall_nits: 300,
        });

        let params = EncoderParams::from([
            ("aq-mode".to_owned(), "3".to_owned()),
            ("zones".to_owned(), "0,100,crf=20".to_owned()),
        ]);

        let encoders = [
            VideoEncoderConfig::Vaapi(VaapiConfig {
                rate_control: RateControl::Vbr {
                    max_bitrate_kbps: 90_000,
                },
                low_power: false,
                params: params.clone(),
                ..VaapiConfig::default()
            }),
            VideoEncoderConfig::Vaapi(VaapiConfig {
                rate_control: RateControl::ConstantQuality { qp: 19 },
                ..VaapiConfig::default()
            }),
            VideoEncoderConfig::Nvenc(NvencConfig {
                gpu_index: 1,
                preset: NvencPreset::P5,
                rate_control: RateControl::Vbr {
                    max_bitrate_kbps: 50_000,
                },
                params: params.clone(),
                ..NvencConfig::default()
            }),
            VideoEncoderConfig::X264(X264Config {
                tune: vec![X264Tune::Film, X264Tune::Grain],
                threads: NonZeroU32::new(6),
                params: params.clone(),
                ..X264Config::default()
            }),
            VideoEncoderConfig::X265(X265Config {
                tune: Some(X265Tune::Animation),
                threads: NonZeroU32::new(2),
                params,
                ..X265Config::default()
            }),
            VideoEncoderConfig::SvtAv1(SvtAv1Config {
                preset: 4,
                low_latency: false,
                threads: NonZeroU32::new(8),
                ..SvtAv1Config::default()
            }),
        ];

        let decoders = populated_decoders();

        let with_encoders = encoders.into_iter().map(|encoder| {
            let mut config = base.clone();
            config.video.encoder = encoder;
            config
        });
        let with_decoders = decoders.into_iter().map(|decoder| {
            let mut config = base.clone();
            config.video.decoder = decoder;
            config
        });

        let custom = with_custom_primaries(&base);

        with_encoders.chain(with_decoders).chain([custom]).collect()
    }

    fn leaf_paths(prefix: &str, value: &Toml, out: &mut BTreeSet<String>) {
        match value {
            Toml::Table(table) => {
                for (key, child) in table {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    leaf_paths(&path, child, out);
                }
            }
            _ => {
                out.insert(prefix.to_owned());
            }
        }
    }

    fn covered(path: &str) -> bool {
        FIELDS.iter().any(|field| {
            let grouped = matches!(
                field.kind,
                Kind::Resolution | Kind::FrameRate | Kind::Pairs | Kind::Primaries
            );
            path == field.path
                || (grouped
                    && path
                        .strip_prefix(field.path)
                        .is_some_and(|rest| rest.starts_with('.')))
        })
    }

    fn satisfy(form: &mut FormModel, field: usize) {
        for condition in FIELDS[field].when {
            let dependency = first_with_path(condition.path);
            satisfy(form, dependency);
            form.set_text(dependency, condition.any_of[0]);
        }
    }

    #[test]
    fn default_config_round_trips_through_the_form() {
        let config = Config::default();

        assert_eq!(form(&config).build().unwrap(), config);
        assert_eq!(client_form(&config).build().unwrap(), config);
    }

    #[test]
    fn each_role_keeps_the_settings_it_owns() {
        for config in populated_configs() {
            let host = form(&config).build().unwrap();
            let client = client_form(&config).build().unwrap();

            assert_eq!(host.video.encoder, config.video.encoder);
            assert_eq!(host.video.codec, config.video.codec);
            assert_eq!(host.video.pixel_format, config.video.pixel_format);
            assert_eq!(host.video.color, config.video.color);
            assert_eq!(host.capture, config.capture);
            assert_eq!(host.scaling.downscale, config.scaling.downscale);
            assert_eq!(host.video.decoder, VideoDecoderConfig::default());
            assert_eq!(host.color.target, Config::default().color.target);
            assert!(!host.video.stats_overlay);

            assert_eq!(client.video.decoder, config.video.decoder);
            assert_eq!(client.color.target, config.color.target);
            assert_eq!(client.scaling.upscale, config.scaling.upscale);
            assert_eq!(client.video.present_mode, config.video.present_mode);
            assert_eq!(client.video.encoder, VideoEncoderConfig::default());
            assert_eq!(client.capture, Config::default().capture);
            assert_eq!(client.video.codec, Config::default().video.codec);

            assert_eq!(host.peers.host_name, config.peers.host_name);
            assert_eq!(host.peers.client_name, None);
            assert_eq!(client.peers.client_name, config.peers.client_name);
            assert_eq!(client.peers.host_name, None);
            assert!(!client.peers.host_on_launch);
            assert_eq!(host.video.bitrate_kbps, config.video.bitrate_kbps);
            assert_eq!(client.video.bitrate_kbps, config.video.bitrate_kbps);
            assert_eq!(host.network, config.network);
            assert_eq!(client.network, config.network);
            assert_eq!(form(&host).build().unwrap(), host);
            assert_eq!(client_form(&client).build().unwrap(), client);
        }
    }

    #[test]
    fn every_serialized_option_has_a_form_field() {
        for config in populated_configs() {
            let mut leaves = BTreeSet::new();
            leaf_paths("", &Toml::Table(to_table(&config).unwrap()), &mut leaves);

            let missing: Vec<_> = leaves.iter().filter(|path| !covered(path)).collect();

            assert!(
                missing.is_empty(),
                "options without a form field: {missing:?}"
            );
        }
    }

    #[test]
    fn every_field_path_names_a_real_option() {
        let mut known = BTreeSet::new();
        for config in populated_configs() {
            leaf_paths("", &Toml::Table(to_table(&config).unwrap()), &mut known);
        }

        for field in FIELDS {
            let exists = known.iter().any(|path| {
                path == field.path
                    || path
                        .strip_prefix(field.path)
                        .is_some_and(|rest| rest.starts_with('.'))
            });

            assert!(exists || field.kind == Kind::Pairs, "{}", field.path);
        }
    }

    #[test]
    fn every_choice_option_is_accepted_by_the_config() {
        for (index, field) in FIELDS.iter().enumerate() {
            let Kind::Choice(options) = field.kind else {
                continue;
            };

            for option in options {
                let mut model = form_as(&Config::default(), role_for(index));
                satisfy(&mut model, index);
                model.set_flag(first_with_path("input.hotkeys"), false);
                model.set_text(index, *option);

                assert!(model.build().is_ok(), "{} = {option}", field.path);
            }
        }
    }

    #[test]
    fn every_named_primaries_option_is_accepted_by_the_config() {
        let fields = [
            first_with_path("video.color.primaries"),
            first_with_path("color.target.primaries"),
            first_with_path("video.color.hdr.mastering.primaries"),
        ];

        for index in fields {
            for name in crate::settings::schema::PRIMARIES {
                let mut model = form_as(&Config::default(), role_for(index));
                model.set_text(index, *name);
                model.set_flag(first_with_path("input.hotkeys"), false);

                assert!(model.build().is_ok(), "{} = {name}", FIELDS[index].path);
            }
        }
    }

    #[test]
    fn custom_primaries_are_kept_and_edited_through_the_form() {
        let custom = populated_configs().pop().unwrap();
        let target = first_with_path("color.target.primaries");
        let host = form(&custom);
        let mut model = client_form(&custom);

        assert!(
            host.text(first_with_path("video.color.primaries"))
                .starts_with("custom ")
        );
        assert_eq!(host.build().unwrap().video.color, custom.video.color);
        assert_eq!(model.build().unwrap().color.target, custom.color.target);

        model.set_text(target, "custom 0.68,0.32 0.265,0.69 0.15,0.06 0.3127,0.329");
        let edited = model.build().unwrap();
        assert_ne!(edited.color.target.primaries, custom.color.target.primaries);
        assert!(matches!(
            edited.color.target.primaries,
            Some(ColorPrimaries::Custom(_))
        ));

        model.set_text(target, "custom 0.1,0.1 0.2,0.2 0.3,0.3 0.3127,0.329");
        assert_eq!(
            model.check_field(target),
            Err(FieldError::PrimariesRejected)
        );
        assert!(model.build().is_err());
    }

    #[test]
    fn every_multi_choice_option_is_accepted_by_the_config() {
        let tune = index_of("video.encoder.tune", "Tune");
        let Kind::MultiChoice(options) = FIELDS[tune].kind else {
            panic!("tune is not a multi choice");
        };

        let mut model = form(&with_encoder(VideoEncoderConfig::X264(
            X264Config::default(),
        )));
        for option in options {
            model.toggle_member(tune, option);
        }

        assert!(model.build().is_ok());
    }

    #[test]
    fn conditions_gate_only_on_tagged_enum_selectors() {
        let selectors = [
            "video.encoder.backend",
            "video.encoder.rate_control.mode",
            "video.decoder.backend",
        ];

        for field in FIELDS {
            for condition in field.when {
                assert!(selectors.contains(&condition.path), "{}", field.path);
            }
        }
    }

    #[test]
    fn saving_and_reloading_reproduces_the_form() {
        let directory =
            std::env::temp_dir().join(format!("rusplay-app-form-{}", std::process::id()));

        for role in [ConfigRole::Host, ConfigRole::Client] {
            for (position, config) in populated_configs().into_iter().enumerate() {
                let path = directory.join(format!("{role:?}-{position}.toml"));
                let built = form_as(&config, role).build().unwrap();
                built.save(&path).unwrap();
                let reloaded = Config::load(&path).unwrap();

                assert_eq!(form_as(&reloaded, role), form_as(&built, role));
                assert_eq!(reloaded, built);
            }
        }

        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn conditions_name_existing_options() {
        for field in FIELDS {
            for condition in field.when {
                let dependency = FIELDS
                    .iter()
                    .find(|candidate| candidate.path == condition.path)
                    .unwrap();
                let Kind::Choice(options) = dependency.kind else {
                    panic!("{} is not a choice", condition.path);
                };

                assert!(condition.any_of.iter().all(|value| options.contains(value)));
            }
        }
    }

    #[test]
    fn the_other_role_s_encode_and_decode_rows_stay_hidden() {
        let config = Config::default();
        let host = form(&config);
        let client = client_form(&config);
        let encoder = first_with_path("video.encoder.backend");
        let decoder = first_with_path("video.decoder.backend");
        let capture = first_with_path("capture.source");
        let present = first_with_path("video.present_mode");
        let launch = first_with_path("peers.host_on_launch");
        let bitrate = first_with_path("video.bitrate_kbps");

        assert!(host.is_visible(encoder));
        assert!(!host.is_visible(decoder));
        assert!(host.is_visible(capture));
        assert!(!host.is_visible(present));
        assert!(host.is_visible(launch));
        assert!(host.is_visible(bitrate));

        assert!(!client.is_visible(encoder));
        assert!(client.is_visible(decoder));
        assert!(!client.is_visible(capture));
        assert!(client.is_visible(present));
        assert!(!client.is_visible(launch));
        assert!(client.is_visible(bitrate));
        assert_eq!(audience("video.encoder.device"), Audience::Host);
        assert_eq!(audience("video.decoder.device"), Audience::Client);
        assert_eq!(audience("network.port"), Audience::Shared);
    }

    #[test]
    fn switching_backend_changes_visible_fields() {
        let backend = first_with_path("video.encoder.backend");
        let device = first_with_path("video.encoder.device");
        let gpu_index = first_with_path("video.encoder.gpu_index");
        let mut model = form(&Config::default());

        assert!(model.is_visible(device));
        assert!(!model.is_visible(gpu_index));

        model.set_text(backend, "nvenc");

        assert!(!model.is_visible(device));
        assert!(model.is_visible(gpu_index));
    }

    #[test]
    fn rate_control_mode_reveals_its_own_field() {
        let mode = first_with_path("video.encoder.rate_control.mode");
        let maximum = first_with_path("video.encoder.rate_control.max_bitrate_kbps");
        let quantizer = first_with_path("video.encoder.rate_control.qp");
        let mut model = form(&Config::default());

        assert!(!model.is_visible(maximum));
        assert!(!model.is_visible(quantizer));

        model.set_text(mode, "vbr");
        assert!(model.is_visible(maximum));
        assert!(!model.is_visible(quantizer));
        assert_eq!(model.text(maximum), "120000");

        model.set_text(mode, "constant-quality");
        assert!(model.is_visible(quantizer));
        assert_eq!(model.text(quantizer), "23");
    }

    #[test]
    fn switching_to_a_software_backend_starts_from_its_defaults() {
        let backend = first_with_path("video.encoder.backend");
        let tune = index_of("video.encoder.tune", "Tune");
        let mut model = form(&Config::default());

        model.set_text(backend, "x264");

        assert_eq!(
            model.value(tune),
            &Value::Set(vec!["zerolatency".to_owned()])
        );
        assert_eq!(
            model.build().unwrap().video.encoder,
            VideoEncoderConfig::X264(X264Config::default())
        );
    }

    #[test]
    fn non_numeric_input_is_reported_on_its_field() {
        let bitrate = first_with_path("video.bitrate_kbps");
        let mut model = form(&Config::default());
        model.set_text(bitrate, "fast");

        let issues = model.build().unwrap_err();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].field(), Some(bitrate));
        assert!(matches!(
            &issues[0],
            FormError::Field {
                error: FieldError::NotAnInteger,
                ..
            }
        ));
    }

    #[test]
    fn numbers_outside_their_type_are_rejected() {
        let port = first_with_path("network.port");
        let mut model = form(&Config::default());

        for text in ["-1", "65536"] {
            model.set_text(port, text);

            assert_eq!(
                model.check_field(port),
                Err(FieldError::OutOfRange { min: 0, max: 65535 })
            );
        }
    }

    #[test]
    fn config_validation_failures_land_on_their_field() {
        let mtu = first_with_path("network.mtu");
        let mut model = form(&Config::default());
        model.set_text(mtu, "100");

        let issues = model.build().unwrap_err();

        assert_eq!(issues[0].field(), Some(mtu));
        assert!(matches!(
            &issues[0],
            FormError::Rejected {
                error: ConfigError::MtuOutOfRange(100),
                ..
            }
        ));
    }

    #[test]
    fn config_validation_reaches_the_variant_specific_field() {
        let backend = first_with_path("video.encoder.backend");
        let preset = index_of("video.encoder.preset", "Preset");
        let svt_preset = FIELDS
            .iter()
            .position(|field| {
                field.path == "video.encoder.preset"
                    && field.kind == Kind::Integer { min: 0, max: 255 }
            })
            .unwrap();
        let mut model = form(&Config::default());
        model.set_text(backend, "svt-av1");
        model.set_text(svt_preset, "14");

        let issues = model.build().unwrap_err();

        assert_ne!(svt_preset, preset);
        assert_eq!(issues[0].field(), Some(svt_preset));
    }

    #[test]
    fn several_field_errors_are_reported_together() {
        let bitrate = first_with_path("video.bitrate_kbps");
        let address = first_with_path("network.bind_address");
        let mut model = form(&Config::default());
        model.set_text(bitrate, "");
        model.set_text(address, "not-an-address");

        let issues = model.build().unwrap_err();
        let fields: Vec<_> = issues.iter().filter_map(FormError::field).collect();

        assert_eq!(fields, vec![bitrate, address]);
    }

    #[test]
    fn a_reused_hotkey_marks_the_row_that_reuses_it() {
        let stats = first_with_path("input.hotkey_stats");
        let disconnect = first_with_path("input.hotkey_disconnect");
        let mut model = client_form(&Config::default());
        model.set_text(disconnect, "s");

        let issues = model.build().unwrap_err();

        assert_ne!(stats, disconnect);
        assert_eq!(issues[0].field(), Some(disconnect));
    }

    #[test]
    fn optional_fields_accept_blank_input() {
        let resolution = first_with_path("video.resolution");
        let mut model = form(&Config::default());
        model.set_text(resolution, "1920x1080");

        assert_eq!(
            model.build().unwrap().video.resolution,
            Some(Resolution::new(1920, 1080))
        );

        model.set_text(resolution, "  ");

        assert_eq!(model.build().unwrap().video.resolution, None);
    }

    #[test]
    fn malformed_resolution_and_frame_rate_are_rejected() {
        let resolution = first_with_path("video.resolution");
        let frame_rate = first_with_path("video.frame_rate");
        let mut model = form(&Config::default());

        model.set_text(resolution, "1920");
        model.set_text(frame_rate, "sixty");

        assert_eq!(
            model.check_field(resolution),
            Err(FieldError::NotAResolution)
        );
        assert_eq!(
            model.check_field(frame_rate),
            Err(FieldError::NotAFrameRate)
        );
    }

    #[test]
    fn fractional_frame_rates_survive() {
        let frame_rate = first_with_path("video.frame_rate");
        let mut model = form(&Config::default());
        model.set_text(frame_rate, "60000/1001");

        assert_eq!(
            model.build().unwrap().video.frame_rate,
            FrameRate::new(60_000, 1001)
        );
    }

    #[test]
    fn encoder_parameters_are_edited_as_pairs() {
        let params = first_with_path("video.encoder.params");
        let mut model = form(&Config::default());

        model.add_pair(params);
        model.set_pair(
            params,
            0,
            Pair {
                key: "rc-lookahead".to_owned(),
                value: "0".to_owned(),
            },
        );
        model.add_pair(params);

        let VideoEncoderConfig::Vaapi(vaapi) = model.build().unwrap().video.encoder else {
            panic!("backend changed");
        };
        assert_eq!(
            vaapi.params,
            EncoderParams::from([("rc-lookahead".to_owned(), "0".to_owned())])
        );

        model.remove_pair(params, 0);
        model.remove_pair(params, 0);
        assert_eq!(model.pairs(params), []);
    }

    #[test]
    fn duplicate_and_nameless_parameters_are_rejected() {
        let params = first_with_path("video.encoder.params");
        let pair = |key: &str, value: &str| Pair {
            key: key.to_owned(),
            value: value.to_owned(),
        };
        let mut model = form(&Config::default());

        model.add_pair(params);
        model.add_pair(params);
        model.set_pair(params, 0, pair("", "1"));
        assert_eq!(
            model.check_field(params),
            Err(FieldError::EmptyParameterName)
        );

        model.set_pair(params, 0, pair("a", "1"));
        model.set_pair(params, 1, pair("a", "2"));
        assert_eq!(
            model.check_field(params),
            Err(FieldError::DuplicateParameter("a".to_owned()))
        );
    }

    #[test]
    fn flags_toggle_through_the_model() {
        let pacing = first_with_path("network.pacing");
        let mut model = form(&Config::default());
        model.set_flag(pacing, false);

        assert!(!model.build().unwrap().network.pacing);
    }
}
