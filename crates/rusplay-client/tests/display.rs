use rusplay_client::{ClientError, query_outputs};
use rusplay_config::DisplayTargetConfig;

#[test]
#[ignore = "needs a running wayland compositor"]
fn the_compositor_describes_its_outputs() {
    let outputs = match query_outputs() {
        Ok(outputs) => outputs,
        Err(ClientError::ColorManagementUnsupported) => return,
        Err(error) => panic!("{error:?}"),
    };

    assert!(!outputs.is_empty());
    for output in outputs {
        let image = output.image.unwrap();
        let target = rusplay_client::resolve_target(&image, &DisplayTargetConfig::default());
        eprintln!("{:?}: {image:?} -> {target:?}", output.name);
        target.unwrap();
    }
}
