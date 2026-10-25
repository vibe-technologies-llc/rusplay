use std::sync::Arc;

use parking_lot::Mutex;
use rusplay_config::PointerMode;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::{BindError, GlobalList, GlobalListContents},
    protocol::{
        wl_keyboard::{self, KeyState, WlKeyboard},
        wl_pointer::{self, Axis, ButtonState, WlPointer},
        wl_registry::WlRegistry,
        wl_seat::{self, Capability, WlSeat},
        wl_surface::WlSurface,
    },
};
use wayland_protocols::wp::{
    pointer_constraints::zv1::client::{
        zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
        zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
    },
    relative_pointer::zv1::client::{
        zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
    },
};

use crate::{
    InputError, InputEvent,
    capture::{CaptureCore, Transition},
};

const SEAT_VERSIONS: std::ops::RangeInclusive<u32> = 1..=8;
const CONSTRAINT_VERSIONS: std::ops::RangeInclusive<u32> = 1..=1;
const WHEEL_UNITS_PER_NOTCH: f64 = 10.0;
const VALUE120_PER_NOTCH: f64 = 120.0;

#[derive(Debug, Clone, Copy)]
pub(super) enum Command {
    Engage,
    Release,
}

#[derive(Debug, Default, Clone, Copy)]
struct ScrollFrame {
    continuous: (f64, f64),
    discrete: (f64, f64),
}

pub(super) struct State {
    core: Arc<Mutex<CaptureCore>>,
    sink: Box<dyn FnMut(InputEvent) + Send>,
    surface: WlSurface,
    handle: QueueHandle<State>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    constraints: Option<ZwpPointerConstraintsV1>,
    relative_manager: Option<ZwpRelativePointerManagerV1>,
    relative: Option<ZwpRelativePointerV1>,
    locked: Option<ZwpLockedPointerV1>,
    pointer_focused: bool,
    keyboard_focused: bool,
    scroll: ScrollFrame,
}

impl State {
    pub(super) fn bind(
        globals: &GlobalList,
        handle: &QueueHandle<State>,
        surface: WlSurface,
        core: Arc<Mutex<CaptureCore>>,
        sink: Box<dyn FnMut(InputEvent) + Send>,
    ) -> Result<Self, InputError> {
        let missing = |interface| {
            move |source: BindError| InputError::WaylandGlobalMissing {
                interface,
                source: Box::new(source),
            }
        };
        let relative_mode = core.lock().options().pointer_mode == PointerMode::Relative;

        globals
            .bind::<WlSeat, _, _>(handle, SEAT_VERSIONS, ())
            .map_err(missing("wl_seat"))?;
        let constraints =
            globals.bind::<ZwpPointerConstraintsV1, _, _>(handle, CONSTRAINT_VERSIONS, ());
        let relative_manager =
            globals.bind::<ZwpRelativePointerManagerV1, _, _>(handle, CONSTRAINT_VERSIONS, ());

        let (constraints, relative_manager) = if relative_mode {
            (
                Some(constraints.map_err(missing("zwp_pointer_constraints_v1"))?),
                Some(relative_manager.map_err(missing("zwp_relative_pointer_manager_v1"))?),
            )
        } else {
            (constraints.ok(), relative_manager.ok())
        };

        Ok(Self {
            core,
            sink,
            surface,
            handle: handle.clone(),
            pointer: None,
            keyboard: None,
            constraints,
            relative_manager,
            relative: None,
            locked: None,
            pointer_focused: false,
            keyboard_focused: false,
            scroll: ScrollFrame::default(),
        })
    }

    pub(super) fn command(&mut self, command: Command) {
        let mut out = Vec::new();
        let transition = {
            let mut core = self.core.lock();
            match command {
                Command::Engage => core.engage(),
                Command::Release => core.release(&mut out),
            }
        };

        self.apply(transition);
        self.deliver(out);
    }

    pub(super) fn shutdown(&mut self) {
        let mut out = Vec::new();
        let transition = self.core.lock().release(&mut out);

        self.apply(transition);
        self.deliver(out);

        if let Some(relative) = self.relative.take() {
            relative.destroy();
        }
        if let Some(pointer) = self.pointer.take().filter(|pointer| pointer.version() >= 3) {
            pointer.release();
        }
        if let Some(keyboard) = self
            .keyboard
            .take()
            .filter(|keyboard| keyboard.version() >= 3)
        {
            keyboard.release();
        }
    }

    fn with_core<R>(&mut self, act: impl FnOnce(&mut CaptureCore, &mut Vec<InputEvent>) -> R) -> R {
        let mut out = Vec::new();
        let result = act(&mut self.core.lock(), &mut out);
        self.deliver(out);

        result
    }

    fn deliver(&mut self, events: Vec<InputEvent>) {
        for event in events {
            (self.sink)(event);
        }
    }

    fn apply(&mut self, transition: Transition) {
        match transition {
            Transition::Engaged => self.lock_pointer(),
            Transition::Released => self.unlock_pointer(),
            Transition::Unchanged => {}
        }
    }

    fn lock_pointer(&mut self) {
        let relative = self.core.lock().options().pointer_mode == PointerMode::Relative;
        let (Some(constraints), Some(pointer), true, None) =
            (&self.constraints, &self.pointer, relative, &self.locked)
        else {
            return;
        };

        self.locked = Some(constraints.lock_pointer(
            &self.surface,
            pointer,
            None,
            Lifetime::Persistent,
            &self.handle,
            (),
        ));
    }

    fn unlock_pointer(&mut self) {
        if let Some(locked) = self.locked.take() {
            locked.destroy();
        }
    }

    fn flush_scroll(&mut self) {
        let frame = std::mem::take(&mut self.scroll);
        let (dx, dy) = if frame.discrete == (0.0, 0.0) {
            (
                frame.continuous.0 / WHEEL_UNITS_PER_NOTCH,
                frame.continuous.1 / WHEEL_UNITS_PER_NOTCH,
            )
        } else {
            frame.discrete
        };

        if dx != 0.0 || dy != 0.0 {
            let focused = self.pointer_focused;
            self.with_core(|core, out| {
                if focused {
                    core.scroll(dx, dy, out);
                }
            });
        }
    }

    fn create_pointer_objects(&mut self, pointer: &WlPointer) {
        if let Some(manager) = &self.relative_manager {
            self.relative = Some(manager.get_relative_pointer(pointer, &self.handle, ()));
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };

        if capabilities.contains(Capability::Pointer) && state.pointer.is_none() {
            let pointer = seat.get_pointer(handle, ());
            state.create_pointer_objects(&pointer);
            state.pointer = Some(pointer);
        }

        if capabilities.contains(Capability::Keyboard) && state.keyboard.is_none() {
            state.keyboard = Some(seat.get_keyboard(handle, ()));
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } if surface == state.surface => {
                state.pointer_focused = true;
                state.with_core(|core, out| core.pointer_position(surface_x, surface_y, out));
            }
            wl_pointer::Event::Leave { surface, .. } if surface == state.surface => {
                state.pointer_focused = false;
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } if state.pointer_focused => {
                state.with_core(|core, out| core.pointer_position(surface_x, surface_y, out));
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(button_state),
                ..
            } if state.pointer_focused => {
                let pressed = button_state == ButtonState::Pressed;
                let transition =
                    state.with_core(|core, out| core.pointer_button(button, pressed, out));
                state.apply(transition);
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(axis),
                value,
                ..
            } => match axis {
                Axis::HorizontalScroll => state.scroll.continuous.0 += value,
                Axis::VerticalScroll => state.scroll.continuous.1 += value,
                _ => {}
            },
            wl_pointer::Event::AxisValue120 {
                axis: WEnum::Value(axis),
                value120,
            } => {
                let notches = f64::from(value120) / VALUE120_PER_NOTCH;
                match axis {
                    Axis::HorizontalScroll => state.scroll.discrete.0 += notches,
                    Axis::VerticalScroll => state.scroll.discrete.1 += notches,
                    _ => {}
                }
            }
            wl_pointer::Event::Frame => state.flush_scroll(),
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } if surface == state.surface => {
                state.keyboard_focused = true;
            }
            wl_keyboard::Event::Leave { surface, .. } if surface == state.surface => {
                state.keyboard_focused = false;
                let transition = state.with_core(CaptureCore::release);
                state.apply(transition);
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(key_state),
                ..
            } if state.keyboard_focused => {
                let pressed = key_state == KeyState::Pressed;
                let transition = state.with_core(|core, out| core.key(key, pressed, out));
                state.apply(transition);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            dx_unaccel,
            dy_unaccel,
            ..
        } = event
        {
            state.with_core(|core, out| core.pointer_motion(dx_unaccel, dy_unaccel, out));
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpLockedPointerV1,
        _: zwp_locked_pointer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpPointerConstraintsV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpPointerConstraintsV1,
        _: <ZwpPointerConstraintsV1 as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpRelativePointerManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpRelativePointerManagerV1,
        _: <ZwpRelativePointerManagerV1 as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
