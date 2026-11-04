macro_rules! flavour {
    (
        $module:ident,
        protocol: $protocol:path,
        manager: $manager_mod:ident :: $Manager:ident,
        device: $device_mod:ident :: $Device:ident,
        source: $source_mod:ident :: $Source:ident,
        offer: $offer_mod:ident :: $Offer:ident,
        global: $global:literal,
        kind: $kind:expr $(,)?
    ) => {
        pub(crate) mod $module {
            use std::{collections::HashMap, sync::Arc, thread, time::Instant};

            use wayland_client::{
                Connection, Dispatch, Proxy, QueueHandle,
                globals::{GlobalListContents, registry_queue_init},
                protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
                backend::ObjectId,
            };
            use $protocol as protocol;
            use protocol::{
                $device_mod::{self, $Device},
                $manager_mod::$Manager,
                $offer_mod::{self, $Offer},
                $source_mod::{self, $Source},
            };

            use crate::{
                BackendKind, ClipboardError,
                data_control::{
                    Flavour, Session, Start, Started,
                },
                transfer::{TEXT_MIME, TRANSFER_TIMEOUT, is_shareable, write_all},
            };

            pub(crate) const KIND: BackendKind = $kind;

            struct State {
                session: Session,
                manager: $Manager,
                device: $Device,
                source: Option<($Source, Arc<str>)>,
                offers: HashMap<ObjectId, Vec<String>>,
                finished: bool,
            }

            pub(crate) fn start(
                connection: &Connection,
                options: Start,
            ) -> Result<Option<Started>, ClipboardError> {
                let (globals, mut queue) = registry_queue_init::<State>(connection)
                    .map_err(|error| ClipboardError::WaylandGlobals(Box::new(error)))?;
                let offered = globals
                    .contents()
                    .with_list(|list| list.iter().any(|global| global.interface == $global));
                if !offered {
                    return Ok(None);
                }

                let handle = queue.handle();
                let seat: WlSeat = globals
                    .bind(&handle, 1..=1, ())
                    .map_err(|_| ClipboardError::NoSeat)?;
                let manager: $Manager = globals
                    .bind(&handle, 1..=1, ())
                    .map_err(|_| ClipboardError::DataControlMissing)?;
                let device = manager.get_data_device(&seat, &handle, ());

                let mut state = State {
                    session: Session::new(options),
                    manager,
                    device,
                    source: None,
                    offers: HashMap::new(),
                    finished: false,
                };
                queue
                    .roundtrip(&mut state)
                    .map_err(|error| ClipboardError::WaylandDispatch(Box::new(error)))?;
                state.session.arm();

                Ok(Some(Started::spawn(connection.clone(), queue, state, KIND)?))
            }

            impl Flavour for State {
                fn session(&mut self) -> &mut Session {
                    &mut self.session
                }

                fn is_finished(&self) -> bool {
                    self.finished
                }

                fn set_text(&mut self, text: &str, handle: &QueueHandle<Self>) {
                    let source = self.manager.create_data_source(handle, ());
                    source.offer(TEXT_MIME.to_owned());
                    self.device.set_selection(Some(&source));

                    if let Some((old, _)) = self.source.replace((source, Arc::from(text))) {
                        old.destroy();
                    }
                }

                fn shut_down(&mut self) {
                    if let Some((source, _)) = self.source.take() {
                        source.destroy();
                    }
                    self.device.destroy();
                }
            }

            impl Dispatch<WlRegistry, GlobalListContents> for State {
                fn event(
                    _: &mut Self,
                    _: &WlRegistry,
                    _: wayland_client::protocol::wl_registry::Event,
                    _: &GlobalListContents,
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }

            impl Dispatch<WlSeat, ()> for State {
                fn event(
                    _: &mut Self,
                    _: &WlSeat,
                    _: wayland_client::protocol::wl_seat::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }

            impl Dispatch<$Manager, ()> for State {
                fn event(
                    _: &mut Self,
                    _: &$Manager,
                    _: <$Manager as Proxy>::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }

            impl Dispatch<$Device, ()> for State {
                fn event(
                    state: &mut Self,
                    _: &$Device,
                    event: $device_mod::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                    match event {
                        $device_mod::Event::DataOffer { id } => {
                            state.offers.insert(id.id(), Vec::new());
                        }
                        $device_mod::Event::Selection { id: Some(offer) } => {
                            let mimes = state.offers.remove(&offer.id()).unwrap_or_default();
                            if state.session.is_armed() && is_shareable(&mimes) {
                                if let Ok(writer) = state.session.begin_read(Instant::now()) {
                                    offer.receive(TEXT_MIME.to_owned(), std::os::fd::AsFd::as_fd(&writer));
                                }
                            }
                            offer.destroy();
                        }
                        $device_mod::Event::PrimarySelection { id: Some(offer) } => {
                            state.offers.remove(&offer.id());
                            offer.destroy();
                        }
                        $device_mod::Event::Finished => state.finished = true,
                        _ => {}
                    }
                }

                wayland_client::event_created_child!(State, $Device, [
                    $device_mod::EVT_DATA_OFFER_OPCODE => ($Offer, ()),
                ]);
            }

            impl Dispatch<$Offer, ()> for State {
                fn event(
                    state: &mut Self,
                    offer: &$Offer,
                    event: $offer_mod::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                    if let $offer_mod::Event::Offer { mime_type } = event {
                        state.offers.entry(offer.id()).or_default().push(mime_type);
                    }
                }
            }

            impl Dispatch<$Source, ()> for State {
                fn event(
                    state: &mut Self,
                    source: &$Source,
                    event: $source_mod::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                    match event {
                        $source_mod::Event::Send { mime_type, fd } => {
                            let current = state
                                .source
                                .as_ref()
                                .filter(|(own, _)| own.id() == source.id())
                                .map(|(_, text)| Arc::clone(text));
                            if let Some(text) = current.filter(|_| mime_type == TEXT_MIME) {
                                thread::spawn(move || {
                                    write_all(&fd, text.as_bytes(), TRANSFER_TIMEOUT).ok();
                                });
                            }
                        }
                        $source_mod::Event::Cancelled => {
                            if state
                                .source
                                .as_ref()
                                .is_some_and(|(own, _)| own.id() == source.id())
                            {
                                state.source = None;
                            }
                            source.destroy();
                        }
                        _ => {}
                    }
                }
            }
        }
    };
}

pub(crate) use flavour;
