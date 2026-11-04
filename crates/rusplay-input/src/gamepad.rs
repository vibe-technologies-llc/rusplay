#[cfg(test)]
mod descriptor_walk;
mod dualsense;
mod dualshock4;
mod hidraw;
mod model;
mod personality;
mod physical;
mod pool;
mod uhid;
mod uinput;
mod xbox360;

pub use self::{
    personality::{PersonalityFactory, VirtualGamepad, VirtualGamepads},
    physical::{PhysicalGamepads, PhysicalOptions},
    pool::{GamepadDevice, GamepadFactory, GamepadPool},
    uhid::UhidGamepad,
    uinput::{UinputFactory, UinputGamepad},
    xbox360::{Report, Xbox360Mapper},
};
