#[cfg(any(
    feature = "x264",
    feature = "x265",
    feature = "svt-av1",
    feature = "vaapi"
))]
mod bindings {
    use std::{env, io, path::PathBuf};

    use bindgen::{
        Builder, EnumVariation, Formatter, MacroTypeVariation, RustEdition,
        callbacks::{ItemInfo, ItemKind, ParseCallbacks},
    };
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

    pub struct Library {
        pub packages: &'static [&'static str],
        pub headers: &'static [&'static str],
        pub macro_type: MacroTypeVariation,
        pub output: &'static str,
        pub symbols: &'static str,
        pub build_suffixed_prefix: Option<&'static str>,
    }

    #[derive(Debug)]
    struct StripBuildNumber {
        prefix: Option<&'static str>,
    }

    impl ParseCallbacks for StripBuildNumber {
        fn generated_name_override(&self, item: ItemInfo<'_>) -> Option<String> {
            let prefix = self.prefix?;

            if item.kind != ItemKind::Function {
                return None;
            }

            let suffix = item.name.strip_prefix(prefix)?;
            let is_build_number = !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit());

            is_build_number.then(|| prefix.trim_end_matches('_').to_owned())
        }
    }

    pub fn generate(library: &Library) -> Result<(), BuildError> {
        let includes = library
            .headers
            .iter()
            .fold(String::new(), |mut includes, header| {
                includes.push_str("#include <");
                includes.push_str(header);
                includes.push_str(">\n");
                includes
            });

        let mut builder = Builder::default()
            .header_contents(
                "wrapper.h",
                &format!("#include <stdint.h>\n#include <stdio.h>\n{includes}"),
            )
            .allowlist_function(library.symbols)
            .allowlist_type(library.symbols)
            .allowlist_var(library.symbols)
            .default_enum_style(EnumVariation::Consts)
            .default_macro_constant_type(library.macro_type)
            .derive_default(true)
            .generate_comments(false)
            .layout_tests(false)
            .formatter(Formatter::None)
            .rust_edition(RustEdition::Edition2024)
            .wrap_unsafe_ops(true)
            .parse_callbacks(Box::new(StripBuildNumber {
                prefix: library.build_suffixed_prefix,
            }));

        for &package in library.packages {
            let probe = pkg_config::Config::new()
                .probe(package)
                .map_err(|source| BuildError::PackageNotFound { package, source })?;

            for path in &probe.include_paths {
                builder = builder.clang_arg(format!("-I{}", path.display()));
            }
        }

        let bindings = builder.generate().map_err(|source| BuildError::Bindgen {
            header: library.headers[0],
            source,
        })?;

        let out_dir = env::var_os("OUT_DIR")
            .map(PathBuf::from)
            .ok_or(BuildError::NoOutDir)?;
        let path = out_dir.join(library.output);

        bindings
            .write_to_file(&path)
            .map_err(|source| BuildError::Write { path, source })
    }
}

#[cfg(any(
    feature = "x264",
    feature = "x265",
    feature = "svt-av1",
    feature = "vaapi"
))]
fn main() -> Result<(), bindings::BuildError> {
    use bindgen::MacroTypeVariation;

    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(feature = "x264")]
    bindings::generate(&bindings::Library {
        packages: &["x264"],
        headers: &["x264.h"],
        macro_type: MacroTypeVariation::Signed,
        output: "x264.rs",
        symbols: "(x264|X264)_.*",
        build_suffixed_prefix: Some("x264_encoder_open_"),
    })?;

    #[cfg(feature = "x265")]
    bindings::generate(&bindings::Library {
        packages: &["x265"],
        headers: &["x265.h"],
        macro_type: MacroTypeVariation::Signed,
        output: "x265.rs",
        symbols: "(x265|X265)_.*",
        build_suffixed_prefix: None,
    })?;

    #[cfg(feature = "svt-av1")]
    bindings::generate(&bindings::Library {
        packages: &["SvtAv1Enc"],
        headers: &["EbSvtAv1Enc.h"],
        macro_type: MacroTypeVariation::Signed,
        output: "svt_av1.rs",
        symbols: "(svt_av1|Svt|Eb|EB|SVT)_?.*",
        build_suffixed_prefix: None,
    })?;

    #[cfg(feature = "vaapi")]
    bindings::generate(&bindings::Library {
        packages: &["libva", "libva-drm"],
        headers: &["va/va.h", "va/va_drm.h", "va/va_drmcommon.h"],
        macro_type: MacroTypeVariation::Unsigned,
        output: "va.rs",
        symbols: "(va|VA|_VA)[A-Z_].*",
        build_suffixed_prefix: None,
    })?;

    Ok(())
}

#[cfg(not(any(
    feature = "x264",
    feature = "x265",
    feature = "svt-av1",
    feature = "vaapi"
)))]
fn main() {}
