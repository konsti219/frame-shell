use crate::{
    gui::{
        panel::{
            GuiPanel, NewGuiPanelParams, apply_custom_command, device_list::DeviceList,
            overlay_list::OverlayList, set_list::SetList,
        },
        timer::GuiTimer,
    },
    state::AppState,
    windowing::{Z_ORDER_WATCH, backend::OverlayEventData, window::OverlayWindowConfig},
};
use glam::{Affine3A, Quat, Vec3, vec3};
use std::{
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};
use wgui::{
    assets::AssetPathRef,
    components::{
        bar_graph::{ComponentBarGraph, ValueCell},
        button::ComponentButton,
    },
    drawing::Color,
    event::{CallbackDataCommon, EventListenerKind},
    i18n::Translation,
    parser::{Fetchable, ParseDocumentParams},
    widget::{EventResult, label::WidgetLabel},
};
use wlx_common::{
    common::LeftRight,
    windowing::{OverlayWindowState, Positioning},
};

pub const WATCH_NAME: &str = "watch";
pub const WATCH_POS: Vec3 = vec3(-0.03, -0.01, 0.125);
pub const WATCH_ROT: Quat = Quat::from_xyzw(-0.707_106_6, 0.000_796_361_8, 0.707_106_6, 0.0);

/// How long the performance section stays revealed after a `reveal_watch` press
const PERF_REVEAL_DURATION: Duration = Duration::from_secs(30);

const WATCH_GRAPH_LOW: Color = Color::new(0.63, 0.90, 0.57, 1.0);
const WATCH_GRAPH_WARN: Color = Color::new(0.96, 0.68, 0.22, 1.0);
const WATCH_GRAPH_HOT: Color = Color::new(0.93, 0.34, 0.22, 1.0);

#[derive(Default)]
struct WatchState {
    device_list: DeviceList,
    overlay_list: OverlayList,
    set_list: SetList,
    clock_12h: bool,
    metrics: Option<WatchMetrics>,
}

/// The watch's frame timings (the presenting PC's runtime while streaming, else the headset's), for markup that has
/// their elements: `fps_current_label`, `fps_avg_label`, `cpu_frametime_graph`, `gpu_frametime_graph`, `net_graph`
/// with `*_value_label`s, `dropped_value_label`, a `perf_section` revealed by `reveal_watch` and a
/// `notification_label`. Everything else on a custom watch comes over IPC (`panel-modify`).
struct WatchMetrics {
    labels: [Option<wgui::layout::WidgetID>; 7],
    graphs: [Option<Rc<ComponentBarGraph>>; 3],
    perf_section: Option<wgui::layout::WidgetID>,
    seen: [usize; 3],
    fps_history: VecDeque<f32>,
    next_fps_sample: Instant,
    perf_visible_until: Option<Instant>,
}

const FPS_CURRENT: usize = 0;
const FPS_AVG: usize = 1;
const CPU_VALUE: usize = 2;
const GPU_VALUE: usize = 3;
const NET_VALUE: usize = 4;
const DROPPED_VALUE: usize = 5;
const NOTIFICATION: usize = 6;

pub fn create_watch(app: &mut AppState) -> anyhow::Result<OverlayWindowConfig> {
    let state = WatchState {
        clock_12h: app.session.config.clock_12h,
        ..Default::default()
    };
    let watch_xml = "gui/watch.xml";

    let mut panel =
        GuiPanel::new_from_template(app, watch_xml, state, NewGuiPanelParams::default())?;

    sets_or_overlays(&mut panel, app);
    init_metrics(&mut panel);

    let doc_params = ParseDocumentParams {
        globals: panel.layout.state.globals.clone(),
        path: AssetPathRef::FileOrBuiltIn(watch_xml),
        extra: panel.doc_extra.take().unwrap_or_default(),
    };

    panel.on_notify = Some(Box::new({
        let name = WATCH_NAME;
        move |panel, app, event_data| {
            let mut elems_changed = panel.state.overlay_list.on_notify(
                app,
                &mut panel.layout,
                &mut panel.parser_state,
                &event_data,
                &doc_params,
            )?;

            elems_changed |= panel.state.set_list.on_notify(
                &mut panel.layout,
                &mut panel.parser_state,
                &event_data,
                &doc_params,
            )?;

            elems_changed |= panel.state.device_list.on_notify(
                app,
                &mut panel.layout,
                &mut panel.parser_state,
                &event_data,
                &doc_params,
            )?;

            match event_data {
                OverlayEventData::EditModeChanged(edit_mode) => {
                    if let Ok(btn_edit_mode) = panel
                        .parser_state
                        .fetch_component_as::<ComponentButton>("btn_edit_mode")
                    {
                        btn_edit_mode.set_sticky_state(&mut panel.layout.common(), edit_mode);
                    }
                }
                OverlayEventData::SettingsChanged => {
                    panel.layout.mark_redraw();
                    sets_or_overlays(panel, app);

                    if app.session.config.clock_12h != panel.state.clock_12h {
                        panel.state.clock_12h = app.session.config.clock_12h;

                        if let Ok(clock_root) = panel.parser_state.get_widget_id("clock_root") {
                            panel.layout.remove_children(clock_root);

                            panel.parser_state.instantiate_template(
                                &doc_params,
                                "Clock",
                                &mut panel.layout,
                                clock_root,
                                Default::default(),
                            )?;

                            elems_changed = true;
                        }
                    }
                }
                OverlayEventData::WatchReveal => {
                    if let Some(metrics) = &mut panel.state.metrics
                        && let Some(section) = metrics.perf_section
                    {
                        metrics.perf_visible_until = Some(Instant::now() + PERF_REVEAL_DURATION);
                        panel.layout.alterables.set_widget_visible(section, true);
                        panel.layout.mark_redraw();
                    }
                }
                OverlayEventData::CustomCommand { element, command } => {
                    if let Err(e) = apply_custom_command(panel, app, &element, &command) {
                        log::warn!("Could not apply {command:?} on {name}/{element}: {e:?}");
                    } else {
                        elems_changed = true;
                    }
                }
                _ => {}
            }

            if elems_changed {
                panel.process_custom_elems(app);
            }

            Ok(())
        }
    }));

    panel
        .timers
        .push(GuiTimer::new(Duration::from_millis(100), 0));

    #[cfg(feature = "openxr")]
    if let Some(monado) = &mut app.monado_state {
        let _ = monado.set_metrics_enabled(true);
    }

    let positioning = Positioning::FollowHand {
        hand: LeftRight::Left,
        lerp: 1.0,
    };

    panel.update_layout(app)?;

    Ok(OverlayWindowConfig {
        name: WATCH_NAME.into(),
        z_order: Z_ORDER_WATCH,
        default_state: OverlayWindowState {
            grabbable: false,
            interactable: true,
            positioning,
            transform: Affine3A::from_scale_rotation_translation(
                Vec3::ONE * 0.115,
                WATCH_ROT,
                WATCH_POS,
            ),
            angle_fade: true,
            ..OverlayWindowState::default()
        },
        show_on_spawn: app.session.config.enable_watch,
        global: true,
        ..OverlayWindowConfig::from_backend(Box::new(panel))
    })
}

fn sets_or_overlays(panel: &mut GuiPanel<WatchState>, app: &mut AppState) {
    let visible = if app.session.config.sets_on_watch {
        [false, true]
    } else {
        [true, false]
    };

    let widget = [
        panel
            .parser_state
            .get_widget_id("panels_root")
            .unwrap_or_default(),
        panel
            .parser_state
            .get_widget_id("sets_root")
            .unwrap_or_default(),
    ];

    for i in 0..2 {
        panel
            .layout
            .alterables
            .set_widget_visible(widget[i], visible[i]);
    }
}

fn init_metrics(panel: &mut GuiPanel<WatchState>) {
    let id = |name: &str| panel.parser_state.get_widget_id(name).ok();
    let labels = [
        "fps_current_label",
        "fps_avg_label",
        "cpu_value_label",
        "gpu_value_label",
        "net_value_label",
        "dropped_value_label",
        "notification_label",
    ]
    .map(id);
    let graph = |name: &str| panel.parser_state.fetch_component_as::<ComponentBarGraph>(name).ok();
    let graphs = [graph("cpu_frametime_graph"), graph("gpu_frametime_graph"), graph("net_graph")];
    let perf_section = id("perf_section");
    if labels.iter().all(Option::is_none) && graphs.iter().all(Option::is_none) {
        return;
    }
    let Ok(root) = panel.parser_state.get_widget_id("watch_panel") else {
        return;
    };
    panel.state.metrics = Some(WatchMetrics {
        labels,
        graphs,
        perf_section,
        seen: [0; 3],
        fps_history: VecDeque::new(),
        next_fps_sample: Instant::now(),
        perf_visible_until: None,
    });
    panel.add_event_listener(
        root,
        EventListenerKind::InternalStateChange,
        Box::new(move |common, _, app, state| {
            if let Some(metrics) = &mut state.metrics {
                metrics.refresh(common, app);
            }
            Ok(EventResult::Pass)
        }),
    );
}

impl WatchMetrics {
    fn label(&self, common: &mut CallbackDataCommon, which: usize, text: &str) {
        if let Some(id) = self.labels[which]
            && let Some(mut label) = common.state.widgets.get_as::<WidgetLabel>(id)
        {
            label.set_text(common, Translation::from_raw_text(text));
        }
    }

    fn refresh(&mut self, common: &mut CallbackDataCommon, app: &mut AppState) {
        #[cfg(feature = "openxr")]
        if let Some(monado) = &mut app.monado_state {
            let _ = monado.set_metrics_enabled(true);
        }
        if let Some(section) = self.perf_section
            && self.perf_visible_until.is_some_and(|until| Instant::now() >= until)
        {
            self.perf_visible_until = None;
            common.alterables.set_widget_visible(section, false);
            common.alterables.mark_redraw();
        }

        let notification = app
            .watch_data
            .latest_notification
            .as_ref()
            .map(|notification| match (notification.title.is_empty(), notification.body.is_empty()) {
                (_, true) => notification.title.clone(),
                (true, _) => notification.body.clone(),
                _ => format!("{}   {}", notification.title, notification.body),
            })
            .unwrap_or_default();
        self.label(common, NOTIFICATION, &notification);

        #[cfg(feature = "openxr")]
        if let Some(metrics) = crate::frame_shell::watch_metrics(app) {
            let now = Instant::now();
            if let Some(fps) = metrics.fps_current
                && now >= self.next_fps_sample
            {
                // One sample a second: the average is over the last minute
                if self.fps_history.len() >= 60 {
                    self.fps_history.pop_front();
                }
                self.fps_history.push_back(fps);
                self.next_fps_sample = now + Duration::from_secs(1);
            }
            let fps_avg = (!self.fps_history.is_empty())
                .then(|| self.fps_history.iter().sum::<f32>() / self.fps_history.len() as f32);
            self.label(common, FPS_CURRENT, &metrics.fps_current.map_or_else(|| "--".into(), |v| format!("{v:.0}")));
            self.label(common, FPS_AVG, &fps_avg.map_or_else(|| "AVG --".into(), |v| format!("AVG {v:.1}")));
            if self.perf_section.is_some() && self.perf_visible_until.is_none() {
                return;
            }
            // Scaled by the frame budget, not measured fps: bars shouldn't shrink as frames get worse
            let budget_ms = metrics
                .display_period_ms
                .or_else(|| fps_avg.map(|fps| 1000.0 / fps.max(1.0)))
                .unwrap_or(12.0)
                .clamp(4.0, 40.0);
            let frame_limits = (0.0, budget_ms * 2.0);
            let series = [
                (&metrics.cpu_ms, metrics.cpu_ms_count, frame_limits),
                (&metrics.gpu_ms, metrics.gpu_ms_count, frame_limits),
                (&metrics.net_ms, metrics.net_ms_count, (0.0, 25.0)),
            ];
            for (i, (values, count, limits)) in series.into_iter().enumerate() {
                if let Some(graph) = &self.graphs[i] {
                    if i < 2 {
                        graph.set_limits(common, limits);
                    }
                    self.seen[i] = push_new_graph_values(graph, values, count, self.seen[i], limits);
                }
            }
            let format_ms = |value: Option<f32>| value.map_or_else(|| "--".into(), |v| format!("{v:.1} ms"));
            self.label(common, CPU_VALUE, &format_ms(metrics.latest_cpu_ms));
            self.label(common, GPU_VALUE, &format_ms(metrics.latest_gpu_ms));
            self.label(common, NET_VALUE, &format_ms(metrics.latest_net_ms));
            self.label(common, DROPPED_VALUE, &metrics.dropped_frames.to_string());
            common.alterables.mark_redraw();
            return;
        }
        for which in [FPS_CURRENT, CPU_VALUE, GPU_VALUE, NET_VALUE, DROPPED_VALUE] {
            self.label(common, which, "--");
        }
        self.label(common, FPS_AVG, "AVG --");
    }
}

fn push_new_graph_values(
    graph: &Rc<ComponentBarGraph>,
    values: &VecDeque<f32>,
    total_count: usize,
    previous_count: usize,
    limits: (f32, f32),
) -> usize {
    let new_count = if total_count < previous_count {
        total_count
    } else {
        total_count - previous_count
    };
    if new_count == 0 {
        return total_count;
    }
    // Only values still in the queue can be pushed; older ones were dropped.
    let to_push = new_count.min(values.len());
    let skip = values.len().saturating_sub(to_push);
    for &value in values.iter().skip(skip) {
        // Sentinel -1 means a dropped frame: render as full-height red bar.
        let (bar_value, color) = if value < 0.0 {
            (limits.1, WATCH_GRAPH_HOT)
        } else {
            (value, graph_color(value, limits))
        };

        graph.push_value(ValueCell {
            value: bar_value,
            color,
        });
    }
    total_count
}

fn graph_color(value: f32, limits: (f32, f32)) -> Color {
    let midpoint = (limits.0 + limits.1) * 0.5;
    if value <= midpoint {
        return WATCH_GRAPH_LOW;
    }

    let t = ((value - midpoint) / (limits.1 - midpoint).max(0.001)).clamp(0.0, 1.0);
    if t < 0.6 {
        WATCH_GRAPH_LOW.lerp(&WATCH_GRAPH_WARN, t / 0.6)
    } else {
        WATCH_GRAPH_WARN.lerp(&WATCH_GRAPH_HOT, (t - 0.6) / 0.4)
    }
}
