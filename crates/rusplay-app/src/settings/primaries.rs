use rusplay_core::ColorPrimaries;
use toml::{Table, Value as Toml};

use crate::settings::{form::FieldError, schema::PRIMARIES};

pub const CUSTOM: &str = "custom";

pub const CHOICES: &[&str] = &[
    "bt709",
    "display-p3",
    "dci-p3",
    "bt2020",
    "bt601-525",
    "bt601-625",
    CUSTOM,
];

const POINT_NAMES: [&str; 4] = ["red", "green", "blue", "white"];
const BT709_SEED: &str = "custom 0.64,0.33 0.30,0.60 0.15,0.06 0.3127,0.329";

pub fn selected(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or_default()
}

pub fn is_custom(text: &str) -> bool {
    selected(text) == CUSTOM
}

pub fn choice(current: &str, option: &str) -> String {
    match option {
        CUSTOM if is_custom(current) => current.to_owned(),
        CUSTOM => BT709_SEED.to_owned(),
        named => named.to_owned(),
    }
}

pub fn text_of(value: &Toml) -> String {
    match value {
        Toml::String(name) => name.clone(),
        Toml::Table(table) => table
            .get(CUSTOM)
            .and_then(Toml::as_table)
            .and_then(custom_text)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn custom_text(points: &Table) -> Option<String> {
    let mut words = vec![CUSTOM.to_owned()];

    for name in POINT_NAMES {
        let [x, y] = points.get(name)?.as_array()?.as_slice() else {
            return None;
        };
        words.push(format!("{},{}", coordinate(x)?, coordinate(y)?));
    }

    Some(words.join(" "))
}

#[allow(clippy::cast_precision_loss)]
fn coordinate(value: &Toml) -> Option<f64> {
    match value {
        Toml::Float(float) => Some(*float),
        Toml::Integer(integer) => Some(*integer as f64),
        _ => None,
    }
}

pub fn value(text: &str) -> Result<Toml, FieldError> {
    let mut words = text.split_whitespace();
    let name = words.next().ok_or(FieldError::Required)?;

    if name != CUSTOM {
        return if words.next().is_none() && PRIMARIES.contains(&name) {
            Ok(Toml::String(name.to_owned()))
        } else if PRIMARIES.contains(&name) {
            Err(FieldError::NotPrimaries)
        } else {
            Err(FieldError::UnknownChoice(name.to_owned()))
        };
    }

    let points: Vec<&str> = words.collect();
    if points.len() != POINT_NAMES.len() {
        return Err(FieldError::NotPrimaries);
    }

    let mut table = Table::new();
    for (name, point) in POINT_NAMES.into_iter().zip(points) {
        let (x, y) = point.split_once(',').ok_or(FieldError::NotPrimaries)?;
        let parse = |text: &str| {
            text.trim()
                .parse::<f64>()
                .ok()
                .filter(|float| float.is_finite())
                .map(Toml::Float)
                .ok_or(FieldError::NotPrimaries)
        };
        table.insert(name.to_owned(), Toml::Array(vec![parse(x)?, parse(y)?]));
    }

    let custom = Toml::Table(Table::from_iter([(CUSTOM.to_owned(), Toml::Table(table))]));
    custom
        .clone()
        .try_into::<ColorPrimaries>()
        .map_err(|_| FieldError::PrimariesRejected)?;

    Ok(custom)
}

#[cfg(test)]
mod tests {
    use rusplay_core::{Chromaticities, Chromaticity, ColorPrimaries};

    use super::*;

    fn round_trip(primaries: ColorPrimaries) -> ColorPrimaries {
        let table = Toml::try_from(primaries).unwrap();

        value(&text_of(&table))
            .unwrap()
            .try_into::<ColorPrimaries>()
            .unwrap()
    }

    #[test]
    fn named_primaries_are_strings() {
        for name in PRIMARIES {
            assert_eq!(value(name).unwrap(), Toml::String((*name).to_owned()));
        }
    }

    #[test]
    fn named_and_custom_primaries_survive_the_text_form() {
        let point = |x, y| Chromaticity::from_xy(x, y).unwrap();
        let custom = ColorPrimaries::Custom(
            Chromaticities::new(
                point(0.68, 0.32),
                point(0.265, 0.69),
                point(0.15, 0.06),
                Chromaticity::D65,
            )
            .unwrap(),
        );

        for primaries in ColorPrimaries::NAMED {
            assert_eq!(round_trip(primaries), primaries);
        }
        assert_eq!(round_trip(custom), custom);
    }

    #[test]
    fn choices_keep_or_seed_the_custom_points() {
        let edited = "custom 0.68,0.32 0.265,0.69 0.15,0.06 0.3127,0.329";

        assert_eq!(choice(edited, CUSTOM), edited);
        assert_eq!(choice("bt2020", CUSTOM), BT709_SEED);
        assert_eq!(choice(edited, "bt2020"), "bt2020");
        assert!(value(BT709_SEED).is_ok());
        assert_eq!(selected(edited), CUSTOM);
        assert!(is_custom(edited));
        assert!(!is_custom("bt709"));
    }

    #[test]
    fn malformed_primaries_are_rejected_with_their_own_errors() {
        assert_eq!(value(""), Err(FieldError::Required));
        assert_eq!(
            value("rec709"),
            Err(FieldError::UnknownChoice("rec709".to_owned()))
        );
        assert_eq!(value("bt709 extra"), Err(FieldError::NotPrimaries));
        assert_eq!(value("custom 0.1,0.1"), Err(FieldError::NotPrimaries));
        assert_eq!(
            value("custom 0.68,0.32 0.265 0.15,0.06 0.3127,0.329"),
            Err(FieldError::NotPrimaries)
        );
        assert_eq!(
            value("custom 0.68,0.32 0.265,x 0.15,0.06 0.3127,0.329"),
            Err(FieldError::NotPrimaries)
        );
        assert_eq!(
            value("custom 0.1,0.1 0.2,0.2 0.3,0.3 0.3127,0.329"),
            Err(FieldError::PrimariesRejected)
        );
    }
}
