use std::{thread, time::Duration};

use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    protocol::wl_registry::{self, WlRegistry},
};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

use super::stack::Stack;

const KEY_HOLD: Duration = Duration::from_millis(40);
const INTERFACE: &str = "org_kde_kwin_fake_input";
const MAX_VERSION: u32 = 5;
const APPLICATION: &str = "rusplay-nested-test";
const REASON: &str = "answer the portal dialog of the nested compositor";

struct Globals {
    input: Option<OrgKdeKwinFakeInput>,
}

impl Dispatch<WlRegistry, ()> for Globals {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == INTERFACE
        {
            state.input = Some(registry.bind(name, version.min(MAX_VERSION), queue, ()));
        }
    }
}

impl Dispatch<OrgKdeKwinFakeInput, ()> for Globals {
    fn event(
        _: &mut Self,
        _: &OrgKdeKwinFakeInput,
        _: <OrgKdeKwinFakeInput as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

pub struct FakeInput {
    input: OrgKdeKwinFakeInput,
    queue: parking_lot::Mutex<EventQueue<Globals>>,
}

impl FakeInput {
    pub fn connect(stack: &Stack) -> Self {
        let connection = Connection::from_socket(stack.connect_wayland()).unwrap();
        let mut queue = connection.new_event_queue();
        let handle = queue.handle();
        let _registry = connection.display().get_registry(&handle, ());
        let mut globals = Globals { input: None };
        queue.roundtrip(&mut globals).unwrap();

        let input = globals
            .input
            .expect("the nested compositor has no fake input");
        input.authenticate(APPLICATION.into(), REASON.into());

        let this = Self {
            input,
            queue: parking_lot::Mutex::new(queue),
        };
        this.flush();
        this
    }

    fn flush(&self) {
        self.queue
            .lock()
            .roundtrip(&mut Globals { input: None })
            .unwrap();
    }

    pub fn pointer_to(&self, x: u32, y: u32) {
        self.input
            .pointer_motion_absolute(f64::from(x), f64::from(y));
        self.flush();
    }

    pub fn press(&self, key: u32) {
        self.input.keyboard_key(key, 1);
        self.flush();
        thread::sleep(KEY_HOLD);
        self.input.keyboard_key(key, 0);
        self.flush();
    }
}
