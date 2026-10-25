use rusplay_core::Resolution;

use super::display_resolution;

const CODED: Resolution = Resolution::new(408, 226);

#[test]
fn render_size_smaller_than_the_coded_size_wins() {
    let render = Resolution::new(402, 226);

    assert_eq!(display_resolution(CODED, Some(render)), render);
}

#[test]
fn render_size_equal_to_the_coded_size_changes_nothing() {
    assert_eq!(display_resolution(CODED, Some(CODED)), CODED);
}

#[test]
fn render_size_larger_than_the_coded_size_is_clamped_to_the_planes() {
    let render = Resolution::new(816, 300);

    assert_eq!(display_resolution(CODED, Some(render)), CODED);
}

#[test]
fn each_dimension_is_clamped_on_its_own() {
    let render = Resolution::new(402, 300);

    assert_eq!(
        display_resolution(CODED, Some(render)),
        Resolution::new(402, 226)
    );
}

#[test]
fn missing_or_empty_render_size_falls_back_to_the_coded_size() {
    assert_eq!(display_resolution(CODED, None), CODED);
    assert_eq!(
        display_resolution(CODED, Some(Resolution::new(0, 0))),
        CODED
    );
}
