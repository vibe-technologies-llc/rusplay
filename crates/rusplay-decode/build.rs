#[cfg(feature = "vaapi")]
mod libva {
    use std::{env, io, path::PathBuf};

    use bindgen::{Builder, EnumVariation, Formatter, MacroTypeVariation, RustEdition};
    use thiserror::Error;

    const PACKAGES: [&str; 2] = ["libva", "libva-drm"];
    const HEADER: &str =
        "#include <va/va.h>\n#include <va/va_drm.h>\n#include <va/va_drmcommon.h>\n";
    const OUTPUT: &str = "va.rs";
    const SYMBOLS: &str = "(va|VA|_VA)[A-Z_].*";

    #[derive(Debug, Error)]
    pub enum BuildError {
        #[error("pkg-config could not find {package}")]
        PackageNotFound {
            package: &'static str,
            source: pkg_config::Error,
        },
        #[error("bindgen failed on the libva headers")]
        Bindgen(#[source] bindgen::BindgenError),
        #[error("OUT_DIR is not set")]
        NoOutDir,
        #[error("could not write {path}")]
        Write { path: PathBuf, source: io::Error },
    }

    pub fn generate() -> Result<(), BuildError> {
        let mut builder = Builder::default()
            .header_contents("wrapper.h", HEADER)
            .allowlist_function(SYMBOLS)
            .allowlist_type(SYMBOLS)
            .allowlist_var(SYMBOLS)
            .default_enum_style(EnumVariation::Consts)
            .default_macro_constant_type(MacroTypeVariation::Unsigned)
            .derive_default(true)
            .generate_comments(false)
            .layout_tests(false)
            .formatter(Formatter::None)
            .rust_edition(RustEdition::Edition2024)
            .wrap_unsafe_ops(true);

        for package in PACKAGES {
            let probe = pkg_config::Config::new()
                .probe(package)
                .map_err(|source| BuildError::PackageNotFound { package, source })?;

            for path in &probe.include_paths {
                builder = builder.clang_arg(format!("-I{}", path.display()));
            }
        }

        let bindings = builder.generate().map_err(BuildError::Bindgen)?;

        let out_dir = env::var_os("OUT_DIR")
            .map(PathBuf::from)
            .ok_or(BuildError::NoOutDir)?;
        let path = out_dir.join(OUTPUT);

        bindings
            .write_to_file(&path)
            .map_err(|source| BuildError::Write { path, source })
    }
}

#[cfg(feature = "vaapi")]
fn main() -> Result<(), libva::BuildError> {
    println!("cargo:rerun-if-changed=build.rs");

    libva::generate()
}

#[cfg(not(feature = "vaapi"))]
fn main() {}
