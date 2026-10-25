use crate::{InputError, InputEvent};

const VALUE120_PER_NOTCH: f64 = 120.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Keyboard,
    Pointer,
    PointerAbsolute,
    Button,
    Scroll,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EiRequest {
    Key { code: u32, pressed: bool },
    MotionRelative { dx: f32, dy: f32 },
    MotionNormalised { x: f32, y: f32 },
    Button { code: u32, pressed: bool },
    Scroll120 { dx: i32, dy: i32 },
}

impl EiRequest {
    pub fn capability(&self) -> Capability {
        match self {
            Self::Key { .. } => Capability::Keyboard,
            Self::MotionRelative { .. } => Capability::Pointer,
            Self::MotionNormalised { .. } => Capability::PointerAbsolute,
            Self::Button { .. } => Capability::Button,
            Self::Scroll120 { .. } => Capability::Scroll,
        }
    }
}

pub fn translate(event: InputEvent) -> Result<EiRequest, InputError> {
    match event {
        InputEvent::Key { keycode, pressed } => Ok(EiRequest::Key {
            code: keycode,
            pressed,
        }),
        InputEvent::PointerMotion { dx, dy } => Ok(EiRequest::MotionRelative {
            dx: narrow(dx)?,
            dy: narrow(dy)?,
        }),
        InputEvent::PointerAbsolute { x, y } => Ok(EiRequest::MotionNormalised {
            x: narrow(x)?.clamp(0.0, 1.0),
            y: narrow(y)?.clamp(0.0, 1.0),
        }),
        InputEvent::PointerButton { button, pressed } => Ok(EiRequest::Button {
            code: button,
            pressed,
        }),
        InputEvent::Scroll { dx, dy } => Ok(EiRequest::Scroll120 {
            dx: notches_to_value120(dx)?,
            dy: notches_to_value120(dy)?,
        }),
        InputEvent::Gamepad { .. } => Err(InputError::GamepadUnsupported),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn narrow(value: f64) -> Result<f32, InputError> {
    if value.is_finite() {
        Ok(value as f32)
    } else {
        Err(InputError::NonFiniteValue)
    }
}

#[allow(clippy::cast_possible_truncation)]
fn notches_to_value120(notches: f64) -> Result<i32, InputError> {
    if !notches.is_finite() {
        return Err(InputError::NonFiniteValue);
    }

    let scaled = (notches * VALUE120_PER_NOTCH).round();
    Ok(scaled.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Region {
    fn right(&self) -> f32 {
        self.x + self.width
    }

    fn bottom(&self) -> f32 {
        self.y + self.height
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MappedRegion {
    pub region: Region,
    pub mapping_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AbsoluteTarget {
    #[default]
    Bounds,
    Mapping(String),
}

impl AbsoluteTarget {
    fn selects(&self, region: &MappedRegion) -> bool {
        match self {
            Self::Bounds => true,
            Self::Mapping(id) => region.mapping_id.as_deref() == Some(id.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbsoluteMapper {
    bounds: Region,
}

impl AbsoluteMapper {
    pub fn new(regions: &[MappedRegion], target: &AbsoluteTarget) -> Result<Self, InputError> {
        let selected: Vec<&Region> = regions
            .iter()
            .filter(|region| target.selects(region))
            .map(|region| &region.region)
            .collect();

        let no_region = || InputError::NoRegion {
            target: target.clone(),
        };

        let first = selected.first().ok_or_else(no_region)?;
        let (left, top, right, bottom) = selected.iter().fold(
            (first.x, first.y, first.right(), first.bottom()),
            |(left, top, right, bottom), region| {
                (
                    left.min(region.x),
                    top.min(region.y),
                    right.max(region.right()),
                    bottom.max(region.bottom()),
                )
            },
        );

        if right <= left || bottom <= top {
            return Err(no_region());
        }

        Ok(Self {
            bounds: Region {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
            },
        })
    }

    pub fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let bounds = &self.bounds;
        let mapped_x =
            (bounds.x + x.clamp(0.0, 1.0) * bounds.width).min(bounds.right().next_down());
        let mapped_y =
            (bounds.y + y.clamp(0.0, 1.0) * bounds.height).min(bounds.bottom().next_down());

        (mapped_x, mapped_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GamepadEvent, InputEvent};

    fn region(x: f32, y: f32, width: f32, height: f32, id: Option<&str>) -> MappedRegion {
        MappedRegion {
            region: Region {
                x,
                y,
                width,
                height,
            },
            mapping_id: id.map(str::to_owned),
        }
    }

    #[test]
    fn keys_and_buttons_keep_their_evdev_codes() {
        let key = translate(InputEvent::Key {
            keycode: 30,
            pressed: true,
        })
        .unwrap();
        let button = translate(InputEvent::PointerButton {
            button: 0x110,
            pressed: false,
        })
        .unwrap();

        assert_eq!(
            key,
            EiRequest::Key {
                code: 30,
                pressed: true
            }
        );
        assert_eq!(
            button,
            EiRequest::Button {
                code: 0x110,
                pressed: false
            }
        );
        assert_eq!(key.capability(), Capability::Keyboard);
        assert_eq!(button.capability(), Capability::Button);
    }

    #[test]
    fn relative_motion_narrows_to_f32() {
        let request = translate(InputEvent::PointerMotion { dx: 2.5, dy: -4.0 }).unwrap();

        assert_eq!(request, EiRequest::MotionRelative { dx: 2.5, dy: -4.0 });
        assert_eq!(request.capability(), Capability::Pointer);
    }

    #[test]
    fn absolute_motion_is_clamped_to_the_unit_square() {
        let request = translate(InputEvent::PointerAbsolute { x: -0.5, y: 3.0 }).unwrap();

        assert_eq!(request, EiRequest::MotionNormalised { x: 0.0, y: 1.0 });
        assert_eq!(request.capability(), Capability::PointerAbsolute);
    }

    #[test]
    fn scroll_notches_become_value120() {
        let request = translate(InputEvent::Scroll { dx: 0.0, dy: -1.5 }).unwrap();

        assert_eq!(request, EiRequest::Scroll120 { dx: 0, dy: -180 });
        assert_eq!(request.capability(), Capability::Scroll);
    }

    #[test]
    fn scroll_saturates_instead_of_wrapping() {
        let request = translate(InputEvent::Scroll {
            dx: 1.0e30,
            dy: -1.0e30,
        })
        .unwrap();

        assert_eq!(
            request,
            EiRequest::Scroll120 {
                dx: i32::MAX,
                dy: i32::MIN
            }
        );
    }

    #[test]
    fn non_finite_values_are_rejected() {
        let motion = translate(InputEvent::PointerMotion {
            dx: f64::NAN,
            dy: 0.0,
        });
        let absolute = translate(InputEvent::PointerAbsolute {
            x: 0.0,
            y: f64::INFINITY,
        });
        let scroll = translate(InputEvent::Scroll {
            dx: f64::NEG_INFINITY,
            dy: 0.0,
        });

        assert!(matches!(motion, Err(InputError::NonFiniteValue)));
        assert!(matches!(absolute, Err(InputError::NonFiniteValue)));
        assert!(matches!(scroll, Err(InputError::NonFiniteValue)));
    }

    #[test]
    fn gamepad_events_are_not_handled() {
        let result = translate(InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::Connected,
        });

        assert!(matches!(result, Err(InputError::GamepadUnsupported)));
    }

    #[test]
    fn single_region_maps_corners_inside_the_region() {
        let mapper = AbsoluteMapper::new(
            &[region(100.0, 50.0, 1920.0, 1080.0, None)],
            &AbsoluteTarget::Bounds,
        )
        .unwrap();

        let origin = mapper.map(0.0, 0.0);
        let centre = mapper.map(0.5, 0.5);
        let far = mapper.map(1.0, 1.0);

        assert_eq!(origin, (100.0, 50.0));
        assert_eq!(centre, (1060.0, 590.0));
        assert!(far.0 < 2020.0 && far.0 > 2019.0);
        assert!(far.1 < 1130.0 && far.1 > 1129.0);
    }

    #[test]
    fn bounds_cover_every_region() {
        let mapper = AbsoluteMapper::new(
            &[
                region(0.0, 0.0, 1920.0, 1080.0, None),
                region(1920.0, 0.0, 1280.0, 1024.0, None),
            ],
            &AbsoluteTarget::Bounds,
        )
        .unwrap();

        assert_eq!(mapper.map(0.5, 0.0), (1600.0, 0.0));
    }

    #[test]
    fn mapping_target_selects_one_region() {
        let regions = [
            region(0.0, 0.0, 1920.0, 1080.0, Some("left")),
            region(1920.0, 0.0, 1280.0, 1024.0, Some("right")),
        ];

        let mapper =
            AbsoluteMapper::new(&regions, &AbsoluteTarget::Mapping("right".into())).unwrap();

        assert_eq!(mapper.map(0.0, 0.0), (1920.0, 0.0));
        assert_eq!(mapper.map(0.5, 0.5), (2560.0, 512.0));
    }

    #[test]
    fn unknown_mapping_has_no_region() {
        let regions = [region(0.0, 0.0, 1920.0, 1080.0, Some("left"))];

        let result = AbsoluteMapper::new(&regions, &AbsoluteTarget::Mapping("right".into()));

        assert!(matches!(result, Err(InputError::NoRegion { .. })));
    }

    #[test]
    fn empty_and_degenerate_regions_have_no_region() {
        let none = AbsoluteMapper::new(&[], &AbsoluteTarget::Bounds);
        let flat = AbsoluteMapper::new(
            &[region(0.0, 0.0, 0.0, 1080.0, None)],
            &AbsoluteTarget::Bounds,
        );

        assert!(matches!(none, Err(InputError::NoRegion { .. })));
        assert!(matches!(flat, Err(InputError::NoRegion { .. })));
    }
}
