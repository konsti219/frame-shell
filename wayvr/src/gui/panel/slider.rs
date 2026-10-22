use std::rc::Rc;

use wgui::{
    components::slider::ComponentSlider,
    parser::{CustomAttribsInfoOwned, ParserState},
};

use crate::{
    gui::panel::{log_cmd_missing_arg, log_invalid_attrib},
    ipc::ipc_server::queue_custom_event,
};

pub(super) fn setup_custom_slider(
    parser_state: &ParserState,
    attribs: &CustomAttribsInfoOwned,
    slider: &Rc<ComponentSlider>,
) {
    const TAG: &str = "Slider";

    let Some(action) = attribs.get_value("_change") else {
        return;
    };
    let mut args = action.split_whitespace();
    match args.next() {
        Some("::IpcSend") => {
            let Some(event) = args.next().map(str::to_owned) else {
                log_cmd_missing_arg(parser_state, TAG, "_change", "::IpcSend");
                return;
            };
            slider.on_value_changed(Box::new(move |_common, changed| {
                queue_custom_event(&event, &changed.value.to_string());
            }));
        }
        _ => log_invalid_attrib(parser_state, TAG, "_change", action),
    }
}
