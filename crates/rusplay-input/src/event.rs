#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GamepadButton(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GamepadAxis(pub u8);

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
pub enum InputEvent {
    Key { keycode: u32, pressed: bool },
    PointerMotion { dx: f64, dy: f64 },
    PointerAbsolute { x: f64, y: f64 },
    PointerButton { button: u32, pressed: bool },
    Scroll { dx: f64, dy: f64 },
    Gamepad { slot: u8, event: GamepadEvent },
}
