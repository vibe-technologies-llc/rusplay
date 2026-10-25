use std::ops::RangeInclusive;

use rusplay_config::DisplayTargetConfig;
use rusplay_scale::ColorVolume;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    globals::{GlobalList, GlobalListContents, registry_queue_init},
    protocol::{
        wl_output::{self, WlOutput},
        wl_registry::WlRegistry,
    },
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1::{self, WpColorManagementOutputV1},
    wp_color_manager_v1::{self, WpColorManagerV1},
    wp_image_description_info_v1::{self, WpImageDescriptionInfoV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};

use crate::{
    ClientError,
    display::{
        DisplayImage, luminances_from_wire, primaries_from_chromaticity, primaries_from_named,
        resolve_target, target_luminance_from_wire, transfer_from_named, transfer_from_power,
    },
};

const MANAGER_VERSIONS: RangeInclusive<u32> = 1..=3;
const MAX_OUTPUT_VERSION: u32 = 4;

#[derive(Debug)]
pub struct OutputDisplay {
    pub name: Option<String>,
    pub image: Result<DisplayImage, ClientError>,
}

pub fn query_outputs() -> Result<Vec<OutputDisplay>, ClientError> {
    let connection = Connection::connect_to_env().map_err(ClientError::WaylandConnect)?;
    let (globals, mut queue) =
        registry_queue_init::<Probe>(&connection).map_err(ClientError::WaylandRegistry)?;
    let handle = queue.handle();
    let manager: WpColorManagerV1 = globals
        .bind(&handle, MANAGER_VERSIONS, ())
        .map_err(|_| ClientError::ColorManagementUnsupported)?;

    let mut probe = Probe {
        outputs: bind_outputs(&globals, &handle),
    };
    probe.request_descriptions(&manager, &handle);
    roundtrip(&mut queue, &mut probe)?;

    probe.request_information(&handle);
    roundtrip(&mut queue, &mut probe)?;

    let outputs = probe.finish();
    manager.destroy();
    let _ = connection.flush();

    Ok(outputs)
}

pub fn query_display(output: Option<&str>) -> Result<DisplayImage, ClientError> {
    let mut outputs = query_outputs()?;
    let index = match output {
        Some(name) => outputs
            .iter()
            .position(|candidate| candidate.name.as_deref() == Some(name))
            .ok_or_else(|| ClientError::OutputNotFound {
                name: name.to_owned(),
            })?,
        None if outputs.is_empty() => return Err(ClientError::NoOutputs),
        None => 0,
    };

    outputs.swap_remove(index).image
}

pub fn query_target(
    output: Option<&str>,
    overrides: &DisplayTargetConfig,
) -> Result<ColorVolume, ClientError> {
    resolve_target(&query_display(output)?, overrides)
}

fn roundtrip(queue: &mut EventQueue<Probe>, probe: &mut Probe) -> Result<(), ClientError> {
    queue
        .roundtrip(probe)
        .map(drop)
        .map_err(ClientError::WaylandDispatch)
}

fn bind_outputs(globals: &GlobalList, handle: &QueueHandle<Probe>) -> Vec<OutputProbe> {
    globals.contents().with_list(|list| {
        list.iter()
            .filter(|global| global.interface == WlOutput::interface().name)
            .enumerate()
            .map(|(index, global)| {
                let version = global.version.min(MAX_OUTPUT_VERSION);
                OutputProbe::new(globals.registry().bind(global.name, version, handle, index))
            })
            .collect()
    })
}

#[derive(Debug)]
enum Progress {
    Requested,
    Ready,
    Described,
    Failed { cause: u32, message: String },
}

struct OutputProbe {
    output: WlOutput,
    name: Option<String>,
    color_output: Option<WpColorManagementOutputV1>,
    description: Option<WpImageDescriptionV1>,
    progress: Progress,
    image: DisplayImage,
}

impl OutputProbe {
    fn new(output: WlOutput) -> Self {
        Self {
            output,
            name: None,
            color_output: None,
            description: None,
            progress: Progress::Requested,
            image: DisplayImage::default(),
        }
    }

    fn into_display(self) -> OutputDisplay {
        let image = match self.progress {
            Progress::Described => Ok(self.image),
            Progress::Failed { cause, message } => {
                Err(ClientError::ImageDescriptionFailed { cause, message })
            }
            Progress::Requested | Progress::Ready => Err(ClientError::ImageInformationIncomplete),
        };

        if let Some(description) = &self.description {
            description.destroy();
        }
        if let Some(color_output) = &self.color_output {
            color_output.destroy();
        }
        if self.output.version() >= wl_output::REQ_RELEASE_SINCE {
            self.output.release();
        }

        OutputDisplay {
            name: self.name,
            image,
        }
    }
}

struct Probe {
    outputs: Vec<OutputProbe>,
}

impl Probe {
    fn request_descriptions(&mut self, manager: &WpColorManagerV1, handle: &QueueHandle<Self>) {
        for (index, probe) in self.outputs.iter_mut().enumerate() {
            let color_output = manager.get_output(&probe.output, handle, index);
            probe.description = Some(color_output.get_image_description(handle, index));
            probe.color_output = Some(color_output);
        }
    }

    fn request_information(&self, handle: &QueueHandle<Self>) {
        for (index, probe) in self.outputs.iter().enumerate() {
            if let (Progress::Ready, Some(description)) = (&probe.progress, &probe.description) {
                description.get_information(handle, index);
            }
        }
    }

    fn finish(self) -> Vec<OutputDisplay> {
        self.outputs
            .into_iter()
            .map(OutputProbe::into_display)
            .collect()
    }

    fn output(&mut self, index: usize) -> Option<&mut OutputProbe> {
        self.outputs.get_mut(index)
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Probe {
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

impl Dispatch<WlOutput, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let (wl_output::Event::Name { name }, Some(probe)) = (event, state.output(*index)) {
            probe.name = Some(name);
        }
    }
}

impl Dispatch<WpColorManagerV1, ()> for Probe {
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

impl Dispatch<WpColorManagementOutputV1, usize> for Probe {
    fn event(
        _: &mut Self,
        _: &WpColorManagementOutputV1,
        _: wp_color_management_output_v1::Event,
        _: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpImageDescriptionV1, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(probe) = state.output(*index) else {
            return;
        };

        match event {
            wp_image_description_v1::Event::Ready { .. }
            | wp_image_description_v1::Event::Ready2 { .. } => probe.progress = Progress::Ready,
            wp_image_description_v1::Event::Failed { cause, msg } => {
                probe.progress = Progress::Failed {
                    cause: cause.into(),
                    message: msg,
                };
            }
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionInfoV1, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &WpImageDescriptionInfoV1,
        event: wp_image_description_info_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(probe) = state.output(*index) else {
            return;
        };
        let image = &mut probe.image;

        match event {
            wp_image_description_info_v1::Event::Primaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                image.primaries =
                    primaries_from_chromaticity([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y])
                        .or(image.primaries);
            }
            wp_image_description_info_v1::Event::PrimariesNamed { primaries } => {
                image.primaries = primaries
                    .into_result()
                    .ok()
                    .and_then(primaries_from_named)
                    .or(image.primaries);
            }
            wp_image_description_info_v1::Event::TargetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                image.target_primaries =
                    primaries_from_chromaticity([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]);
            }
            wp_image_description_info_v1::Event::TfNamed { tf } => {
                image.transfer = tf.into_result().ok().and_then(transfer_from_named);
            }
            wp_image_description_info_v1::Event::TfPower { eexp } => {
                image.transfer = transfer_from_power(eexp);
            }
            wp_image_description_info_v1::Event::Luminances {
                min_lum,
                max_lum,
                reference_lum,
            } => image.luminances = luminances_from_wire(min_lum, max_lum, reference_lum),
            wp_image_description_info_v1::Event::TargetLuminance { min_lum, max_lum } => {
                image.target_luminance = target_luminance_from_wire(min_lum, max_lum);
            }
            wp_image_description_info_v1::Event::Done => probe.progress = Progress::Described,
            _ => {}
        }
    }
}
