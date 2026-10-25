mod pool;
mod uinput;
mod xbox360;

pub use self::{
    pool::{GamepadDevice, GamepadFactory, GamepadPool},
    uinput::{UinputFactory, UinputGamepad, UinputGamepads},
    xbox360::{Report, Xbox360Mapper},
};
