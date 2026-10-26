use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output::WlOutput, wl_registry::WlRegistry},
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1::{self, WpColorManagementOutputV1},
    wp_color_manager_v1::{self, WpColorManagerV1},
    wp_image_description_info_v1::{self, WpImageDescriptionInfoV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};

use crate::{
    ClientError, DisplayImage, Stage,
    display::{MANAGER_VERSIONS, apply_information},
    window::{choice::follow_choice, surface::ChildSurface},
};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const THREAD_NAME: &str = "rusplay-client-output";

pub(crate) struct DisplayFollower {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), ClientError>>>,
}

impl DisplayFollower {
    pub(crate) fn start(
        surface: Arc<ChildSurface>,
        publish: impl FnMut(DisplayImage) + Send + 'static,
    ) -> Result<Self, ClientError> {
        let (globals, queue) = registry_queue_init::<Follow>(surface.connection())
            .map_err(ClientError::WaylandRegistry)?;
        let manager: WpColorManagerV1 = globals
            .bind(&queue.handle(), MANAGER_VERSIONS, ())
            .map_err(|_| ClientError::ColorManagementUnsupported)?;
        let state = Follow::new(manager, Box::new(publish));

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || serve(&surface, queue, state, &worker_stop))
            .map_err(|source| ClientError::SpawnStage {
                stage: Stage::Window,
                source,
            })?;

        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub(crate) fn stop(mut self) -> Result<(), ClientError> {
        self.shut_down()
    }

    fn shut_down(&mut self) -> Result<(), ClientError> {
        self.stop.store(true, Ordering::Relaxed);

        match self.thread.take() {
            Some(thread) => thread.join().unwrap_or(Err(ClientError::StagePanicked {
                stage: Stage::Window,
            })),
            None => Ok(()),
        }
    }
}

impl Drop for DisplayFollower {
    fn drop(&mut self) {
        self.shut_down().ok();
    }
}

fn serve(
    surface: &ChildSurface,
    mut queue: EventQueue<Follow>,
    mut state: Follow,
    stop: &AtomicBool,
) -> Result<(), ClientError> {
    let connection = surface.connection();

    while !stop.load(Ordering::Relaxed) {
        surface.dispatch()?;
        queue
            .dispatch_pending(&mut state)
            .map_err(ClientError::WindowDispatch)?;
        state.follow(&surface.entered_outputs(), &queue.handle());
        connection.flush().map_err(ClientError::OutputFollowRead)?;

        let Some(guard) = connection.prepare_read() else {
            continue;
        };

        let limit = Timespec {
            tv_sec: 0,
            tv_nsec: i64::from(POLL_INTERVAL.subsec_nanos()),
        };
        let ready = {
            let socket = guard.connection_fd();
            let mut fds = [PollFd::new(&socket, PollFlags::IN)];
            match poll(&mut fds, Some(&limit)) {
                Ok(count) => count > 0,
                Err(Errno::INTR) => false,
                Err(errno) => return Err(ClientError::OutputFollowPoll(errno.into())),
            }
        };

        if ready {
            guard.read().map_err(ClientError::OutputFollowRead)?;
        }
    }

    state.release();
    connection.flush().map_err(ClientError::OutputFollowRead)
}

struct Followed {
    output: WlOutput,
    color_output: WpColorManagementOutputV1,
    color_generation: u64,
    description: Option<WpImageDescriptionV1>,
}

struct Follow {
    manager: WpColorManagerV1,
    followed: Option<Followed>,
    generation: u64,
    image: DisplayImage,
    publish: Box<dyn FnMut(DisplayImage) + Send>,
}

impl Follow {
    fn new(manager: WpColorManagerV1, publish: Box<dyn FnMut(DisplayImage) + Send>) -> Self {
        Self {
            manager,
            followed: None,
            generation: 0,
            image: DisplayImage::default(),
            publish,
        }
    }

    fn follow(&mut self, entered: &[WlOutput], handle: &QueueHandle<Self>) {
        let current = self.followed.as_ref().map(|followed| &followed.output);
        let choice = follow_choice(entered, current);

        if let Some(output) = choice
            && current != Some(&output)
        {
            self.switch(output, handle);
        }
    }

    fn switch(&mut self, output: WlOutput, handle: &QueueHandle<Self>) {
        self.release_output();
        self.generation += 1;

        let color_generation = self.generation;
        let color_output = self.manager.get_output(&output, handle, color_generation);
        self.followed = Some(Followed {
            output,
            color_output,
            color_generation,
            description: None,
        });
        self.describe(handle);
    }

    fn describe(&mut self, handle: &QueueHandle<Self>) {
        self.generation += 1;
        self.image = DisplayImage::default();

        let generation = self.generation;
        if let Some(followed) = &mut self.followed {
            if let Some(stale) = followed.description.take() {
                stale.destroy();
            }
            followed.description = Some(
                followed
                    .color_output
                    .get_image_description(handle, generation),
            );
        }
    }

    fn release_output(&mut self) {
        if let Some(followed) = self.followed.take() {
            if let Some(description) = followed.description {
                description.destroy();
            }
            followed.color_output.destroy();
        }
    }

    fn release(&mut self) {
        self.release_output();
        self.manager.destroy();
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Follow {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpColorManagerV1, ()> for Follow {
    fn event(
        _: &mut Self,
        _: &WpColorManagerV1,
        _: wp_color_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpColorManagementOutputV1, u64> for Follow {
    fn event(
        state: &mut Self,
        _: &WpColorManagementOutputV1,
        event: wp_color_management_output_v1::Event,
        generation: &u64,
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        let current = state
            .followed
            .as_ref()
            .is_some_and(|followed| followed.color_generation == *generation);

        if current
            && matches!(
                event,
                wp_color_management_output_v1::Event::ImageDescriptionChanged
            )
        {
            state.describe(handle);
        }
    }
}

impl Dispatch<WpImageDescriptionV1, u64> for Follow {
    fn event(
        state: &mut Self,
        description: &WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        generation: &u64,
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if *generation != state.generation {
            return;
        }

        if matches!(
            event,
            wp_image_description_v1::Event::Ready { .. }
                | wp_image_description_v1::Event::Ready2 { .. }
        ) {
            description.get_information(handle, *generation);
        }
    }
}

impl Dispatch<WpImageDescriptionInfoV1, u64> for Follow {
    fn event(
        state: &mut Self,
        _: &WpImageDescriptionInfoV1,
        event: wp_image_description_info_v1::Event,
        generation: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if *generation != state.generation || !apply_information(&mut state.image, event) {
            return;
        }

        if let Some(description) = state
            .followed
            .as_mut()
            .and_then(|followed| followed.description.take())
        {
            description.destroy();
        }
        (state.publish)(state.image);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use wayland_client::{globals::registry_queue_init, protocol::wl_output};

    use super::*;

    impl Dispatch<WlOutput, ()> for Follow {
        fn event(
            _: &mut Self,
            _: &WlOutput,
            _: wl_output::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    const ROUNDTRIPS: usize = 8;

    #[test]
    #[ignore = "needs a running wayland compositor with colour management"]
    fn a_followed_output_publishes_its_description() {
        let connection = Connection::connect_to_env().unwrap();
        let (globals, mut queue) = registry_queue_init::<Follow>(&connection).unwrap();
        let handle = queue.handle();
        let manager: WpColorManagerV1 = match globals.bind(&handle, MANAGER_VERSIONS, ()) {
            Ok(manager) => manager,
            Err(_) => return,
        };
        let output: WlOutput = globals.bind(&handle, 1..=4, ()).unwrap();

        let published = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&published);
        let mut state = Follow::new(manager, Box::new(move |image| sink.lock().push(image)));

        state.follow(std::slice::from_ref(&output), &handle);
        for _ in 0..ROUNDTRIPS {
            queue.roundtrip(&mut state).unwrap();
            if !published.lock().is_empty() {
                break;
            }
        }

        let images = published.lock().clone();
        eprintln!("{images:?}");
        let first = images.first().unwrap();
        assert!(first.primaries.is_some() || first.transfer.is_some());

        state.release();
        connection.flush().unwrap();
    }
}
