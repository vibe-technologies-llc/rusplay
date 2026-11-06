#[cfg(feature = "ffmpeg")]
mod bindings {
    use std::{env, io, path::PathBuf};

    use bindgen::{Builder, EnumVariation, Formatter, MacroTypeVariation, RustEdition};
    use thiserror::Error;

    #[derive(Debug, Error)]
    pub enum BuildError {
        #[error("pkg-config could not find {package}")]
        PackageNotFound {
            package: &'static str,
            source: pkg_config::Error,
        },
        #[error("bindgen failed on {header}")]
        Bindgen {
            header: &'static str,
            source: bindgen::BindgenError,
        },
        #[error("OUT_DIR is not set")]
        NoOutDir,
        #[error("could not write {path}")]
        Write { path: PathBuf, source: io::Error },
    }

    const WRAPPER: &str = concat!(
        "#include <stdint.h>\n",
        "#include <libavcodec/avcodec.h>\n",
        "#include <libavutil/mastering_display_metadata.h>\n",
        "enum RusplayAvError {\n",
        "    RUSPLAY_AVERROR_EAGAIN = AVERROR(EAGAIN),\n",
        "    RUSPLAY_AVERROR_EOF = AVERROR_EOF\n",
        "};\n",
    );

    const FUNCTIONS: &str = concat!(
        "^avcodec_find_decoder$|^avcodec_alloc_context3$|^avcodec_open2$|",
        "^avcodec_free_context$|^avcodec_send_packet$|^avcodec_receive_frame$|",
        "^av_packet_alloc$|^av_packet_free$|^av_packet_unref$|",
        "^av_frame_alloc$|^av_frame_free$|^av_frame_unref$|^av_frame_get_side_data$",
    );

    const TYPES: &str = concat!(
        "^AVFrame$|^AVPacket$|^AVCodecContext$|^AVCodec$|^AVRational$|",
        "^AVFrameSideData$|^AVMasteringDisplayMetadata$|^AVContentLightMetadata$|",
        "^RusplayAvError$|",
        "^AVPixelFormat$|^AVColorRange$|^AVColorPrimaries$|^AVColorTransferCharacteristic$|",
        "^AVColorSpace$|^AVChromaLocation$|^AVCodecID$|^AVFrameSideDataType$",
    );

    const CONSTANTS: &str =
        "^AV_INPUT_BUFFER_PADDING_SIZE$|^FF_THREAD_SLICE$|^AV_FRAME_FLAG_CORRUPT$";

    pub fn generate() -> Result<(), BuildError> {
        let mut builder = Builder::default()
            .header_contents("wrapper.h", WRAPPER)
            .allowlist_function(FUNCTIONS)
            .allowlist_type(TYPES)
            .allowlist_var(CONSTANTS)
            .opaque_type("AVCodec")
            .default_enum_style(EnumVariation::Consts)
            .prepend_enum_name(false)
            .default_macro_constant_type(MacroTypeVariation::Signed)
            .derive_default(false)
            .generate_comments(false)
            .layout_tests(false)
            .formatter(Formatter::None)
            .rust_edition(RustEdition::Edition2024)
            .wrap_unsafe_ops(true);

        for package in ["libavcodec", "libavutil"] {
            let probe = pkg_config::Config::new()
                .probe(package)
                .map_err(|source| BuildError::PackageNotFound { package, source })?;

            for path in &probe.include_paths {
                builder = builder.clang_arg(format!("-I{}", path.display()));
            }
        }

        let bindings = builder.generate().map_err(|source| BuildError::Bindgen {
            header: "libavcodec/avcodec.h",
            source,
        })?;

        let out_dir = env::var_os("OUT_DIR")
            .map(PathBuf::from)
            .ok_or(BuildError::NoOutDir)?;
        let path = out_dir.join("ffmpeg.rs");

        bindings
            .write_to_file(&path)
            .map_err(|source| BuildError::Write { path, source })
    }
}

#[cfg(feature = "ffmpeg")]
fn main() -> Result<(), bindings::BuildError> {
    println!("cargo:rerun-if-changed=build.rs");
    bindings::generate()
}

#[cfg(not(feature = "ffmpeg"))]
fn main() {}
