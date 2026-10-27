use crate::LockState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GamepadButton(pub u16);

impl GamepadButton {
    pub const SOUTH: Self = Self(0);
    pub const EAST: Self = Self(1);
    pub const WEST: Self = Self(2);
    pub const NORTH: Self = Self(3);
    pub const BACK: Self = Self(4);
    pub const GUIDE: Self = Self(5);
    pub const START: Self = Self(6);
    pub const LEFT_STICK: Self = Self(7);
    pub const RIGHT_STICK: Self = Self(8);
    pub const LEFT_SHOULDER: Self = Self(9);
    pub const RIGHT_SHOULDER: Self = Self(10);
    pub const DPAD_UP: Self = Self(11);
    pub const DPAD_DOWN: Self = Self(12);
    pub const DPAD_LEFT: Self = Self(13);
    pub const DPAD_RIGHT: Self = Self(14);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GamepadAxis(pub u8);

impl GamepadAxis {
    pub const LEFT_X: Self = Self(0);
    pub const LEFT_Y: Self = Self(1);
    pub const RIGHT_X: Self = Self(2);
    pub const RIGHT_Y: Self = Self(3);
    pub const LEFT_TRIGGER: Self = Self(4);
    pub const RIGHT_TRIGGER: Self = Self(5);
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GamepadEvent {
    Connected,
    Disconnected,
    Button {
        button: GamepadButton,
        pressed: bool,
    },
    Axis {
        axis: GamepadAxis,
        value: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchEvent {
    Down { x: f64, y: f64 },
    Motion { x: f64, y: f64 },
    Up,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    Key { keycode: u32, pressed: bool },
    PointerMotion { dx: f64, dy: f64 },
    PointerAbsolute { x: f64, y: f64 },
    PointerButton { button: u32, pressed: bool },
    Scroll { dx: f64, dy: f64 },
    Gamepad { slot: u8, event: GamepadEvent },
    Locks(LockState),
    Touch { slot: u8, event: TouchEvent },
}
