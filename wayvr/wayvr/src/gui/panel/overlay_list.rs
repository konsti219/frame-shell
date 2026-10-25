use std::rc::Rc;

use slotmap::{Key, SecondaryMap};
use wgui::{
    components::button::ComponentButton,
    layout::Layout,
    parser::{Fetchable, ParseDocumentParams, ParserState, TemplateParams},
};
use wlx_common::steam;

use crate::windowing::{OverlayID, backend::OverlayEventData, window::OverlayCategory};
use crate::{state::AppState, windowing::backend::OverlayMeta};

#[derive(Default)]
/// Helper for managing a list of overlays
/// Populates `id="panels_root"` with `<Screen>`, `<Mirror>`, `<Panel>` templates
/// Populates `id="apps_root"` with `<App>` templates (optional)
/// Uses the following parameters: `name` (All), `display` (Screen, Mirror), `icon` (App, Panel)
pub struct OverlayList {
    overlay_buttons: SecondaryMap<OverlayID, Rc<ComponentButton>>,
    last_metas: Rc<[OverlayMeta]>,
}

impl OverlayList {
    pub fn on_notify(
        &mut self,
        app: &mut AppState,
        layout: &mut Layout,
        parser_state: &mut ParserState,
        event_data: &OverlayEventData,
        doc_params: &ParseDocumentParams,
    ) -> anyhow::Result<bool> {
        fn rebuild_pins_only(
            app: &mut AppState,
            layout: &mut Layout,
            parser_state: &mut ParserState,
            doc_params: &ParseDocumentParams,
        ) -> anyhow::Result<()> {
            let apps_root = parser_state.get_widget_id("apps_root").unwrap_or_default();
            if apps_root.is_null() {
                return Ok(());
            }

            layout.remove_children(apps_root);

            for (i, _) in app.session.config.pinned_games.iter().enumerate() {
                if !crate::frame_shell::EMBEDDED.load(std::sync::atomic::Ordering::Relaxed) {
                    add_pin(app, layout, parser_state, doc_params, apps_root, i)?;
                }
            }

            Ok(())
        }

        fn rebuild_buttons(
            app: &mut AppState,
            me: &mut OverlayList,
            layout: &mut Layout,
            parser_state: &mut ParserState,
            doc_params: &ParseDocumentParams,
        ) -> anyhow::Result<()> {
            let panels_root = parser_state
                .get_widget_id("panels_root")
                .unwrap_or_default();
            let apps_root = parser_state.get_widget_id("apps_root").unwrap_or_default();

            layout.remove_children(panels_root);
            me.overlay_buttons.clear();

            rebuild_pins_only(app, layout, parser_state, doc_params)?;

            for (i, meta) in me.last_metas.iter().enumerate() {
                let mut params = TemplateParams::new();

                let (template, root) = match meta.category {
                    // The frame shell groups screens by PC (rebuild_groups)
                    OverlayCategory::Screen
                        if crate::frame_shell::EMBEDDED
                            .load(std::sync::atomic::Ordering::Relaxed) =>
                    {
                        continue;
                    }
                    OverlayCategory::Screen => {
                        params.insert(
                            "keep_visible_display",
                            if meta.keep_visible_when_hidden {
                                "flex"
                            } else {
                                "none"
                            },
                        );
                        params.insert_rc(
                            "display",
                            format!(
                                "{}{}",
                                (*meta.name).chars().next().unwrap_or_default(),
                                (*meta.name).chars().last().unwrap_or_default()
                            )
                            .into(),
                        );
                        ("Screen", panels_root)
                    }
                    OverlayCategory::Mirror => {
                        params.insert_rc(
                            "display",
                            (*meta.name).chars().last().unwrap().to_string().into(),
                        );
                        ("Mirror", panels_root)
                    }
                    OverlayCategory::Passthru => {
                        let display = meta
                            .name
                            .rsplit('-')
                            .next()
                            .unwrap_or(&meta.name)
                            .to_string();
                        params.insert_rc("display", display.into());
                        ("Passthru", panels_root)
                    }
                    OverlayCategory::Panel | OverlayCategory::BuiltInPanel => {
                        let icon: Rc<str> = if let Some(icon) = meta.icon.as_ref() {
                            icon.to_string().into()
                        } else {
                            "@/edit/panel.svg".into()
                        };

                        params.insert_rc("icon", icon);
                        params.insert("icon_builtin", "@/edit/panel.svg");
                        ("Panel", panels_root)
                    }
                    OverlayCategory::WayVR => {
                        params.insert(
                            "icon",
                            meta.icon
                                .as_ref()
                                .expect("WayVR overlay without Icon attribute!")
                                .as_ref(),
                        );
                        params.insert("icon_builtin", "@/edit/panel.svg");
                        ("App", apps_root)
                    }
                    OverlayCategory::Dashboard | OverlayCategory::Keyboard => {
                        let key = if matches!(meta.category, OverlayCategory::Dashboard) {
                            "btn_dashboard"
                        } else {
                            "btn_keyboard"
                        };

                        let Ok(overlay_button) =
                            parser_state.fetch_component_as::<ComponentButton>(key)
                        else {
                            continue;
                        };

                        overlay_button
                            .set_sticky_state_immediate(&mut layout.common(), meta.visible);
                        me.overlay_buttons.insert(meta.id, overlay_button);
                        continue;
                    }
                    _ => continue,
                };

                if root.is_null() {
                    continue;
                }

                params.insert_rc("idx", i.to_string().into());
                params.insert("name", meta.name.as_ref());
                parser_state.instantiate_template(doc_params, template, layout, root, params)?;
                let overlay_button =
                    parser_state.fetch_component_as::<ComponentButton>(&format!("overlay_{i}"))?;
                overlay_button.set_sticky_state_immediate(&mut layout.common(), meta.visible);
                me.overlay_buttons.insert(meta.id, overlay_button);
            }

            if crate::frame_shell::EMBEDDED.load(std::sync::atomic::Ordering::Relaxed) {
                rebuild_groups(app, me, layout, parser_state, doc_params)?;
            }

            Ok(())
        }

        let mut elements_changed = false;
        match event_data {
            OverlayEventData::OverlaysChanged(metas) => {
                self.last_metas = metas.clone();
                rebuild_buttons(app, self, layout, parser_state, doc_params)?;
                elements_changed = true;
            }
            OverlayEventData::SettingsChanged => {
                if crate::frame_shell::EMBEDDED.load(std::sync::atomic::Ordering::Relaxed) {
                    rebuild_buttons(app, self, layout, parser_state, doc_params)?;
                } else {
                    rebuild_pins_only(app, layout, parser_state, doc_params)?;
                }
                elements_changed = true;
            }
            OverlayEventData::VisibleOverlaysChanged(overlays) => {
                let mut overlay_buttons = self.overlay_buttons.clone();

                for visible in overlays.as_ref() {
                    if let Some(btn) = overlay_buttons.remove(*visible) {
                        btn.set_sticky_state_immediate(&mut layout.common(), true);
                    }
                }

                for btn in overlay_buttons.values() {
                    btn.set_sticky_state_immediate(&mut layout.common(), false);
                }
            }
            _ => {}
        }

        Ok(elements_changed)
    }
}

/// Adds pin `i` to `root`; false when its game isn't running
fn add_pin(
    app: &AppState,
    layout: &mut Layout,
    parser_state: &mut ParserState,
    doc_params: &ParseDocumentParams,
    root: wgui::layout::WidgetID,
    i: usize,
) -> anyhow::Result<bool> {
    let pin = &app.session.config.pinned_games[i];
    let running = pin.host.as_ref().map_or_else(
        || {
            steam::list_running_games()
                .unwrap_or_default()
                .iter()
                .any(|g| g.app_id == pin.app_id)
        },
        |host| crate::frame_shell::remote_game_running(host, &pin.app_id),
    );

    let (source, clients) = pin.host.as_ref().map_or_else(
        || (None, crate::frame_shell::native_clients(app)),
        |name| {
            crate::frame_shell::top_row_hosts()
                .into_iter()
                .find(|host| host.cookie == *name)
                .map_or((None, Vec::new()), |host| (host.source, host.clients))
        },
    );
    if let Some(client) = clients
        .iter()
        .find(|c| c.app_id.as_deref() == Some(&pin.app_id))
    {
        add_client(
            layout,
            parser_state,
            doc_params,
            root,
            100_000 + i,
            source,
            client,
            Some(i),
        )?;
        return Ok(true);
    }
    let mut params = TemplateParams::new();
    params.insert_rc("idx", i.to_string().into());
    params.insert("name", pin.name.as_str());
    params.insert_rc(
        "icon",
        pin.icon
            .as_ref()
            .map_or_else(|| "".into(), |x| x.as_str().into()),
    );
    params.insert("icon_builtin", "@/edit/3d.svg");
    params.insert("running_skip", if running { "0" } else { "1" });
    params.insert("app_id", &pin.app_id);
    params.insert_rc(
        "source",
        source
            .map_or_else(|| "native".to_owned(), |s| s.to_string())
            .into(),
    );
    parser_state.instantiate_template(doc_params, "PinnedApp", layout, root, params)?;

    let pin_button = parser_state.fetch_component_as::<ComponentButton>(&format!("pinned_{i}"))?;
    if running {
        pin_button.set_sticky_state(&mut layout.common(), true);
    }
    Ok(running)
}

/// Left selects the default app or toggles an overlay; right opens its controls.
fn add_client(
    layout: &mut Layout,
    parser_state: &mut ParserState,
    doc_params: &ParseDocumentParams,
    root: wgui::layout::WidgetID,
    idx: usize,
    source: Option<u32>,
    client: &crate::frame_shell::RunningClient,
    pin: Option<usize>,
) -> anyhow::Result<()> {
    let mut params = TemplateParams::new();
    params.insert_rc("idx", idx.to_string().into());
    let role = if client.overlay { "Overlay" } else { "VR app" };
    params.insert("name", &format!("{} · {role}", client.name));
    params.insert(
        "overlay_display",
        if client.overlay { "flex" } else { "none" },
    );
    params.insert(
        "auto_label",
        &format!(
            "Auto-detect: {role}{}",
            if client.presentation_mode == 0 {
                " ✓"
            } else {
                ""
            }
        ),
    );
    params.insert(
        "default_label",
        if client.presentation_mode == 1 {
            "Force VR app ✓"
        } else {
            "Force VR app"
        },
    );
    params.insert(
        "overlay_label",
        if client.presentation_mode == 2 {
            "Force overlay ✓"
        } else {
            "Force overlay"
        },
    );
    params.insert("show", client.show.as_str());
    params.insert("pin_skip", if pin.is_some() { "0" } else { "1" });
    params.insert_rc("pin", pin.unwrap_or_default().to_string().into());
    params.insert_rc("client", client.client_id.to_string().into());
    params.insert_rc(
        "source",
        source
            .map_or_else(|| "native".to_owned(), |s| s.to_string())
            .into(),
    );
    params.insert("stop_skip", if client.app_id.is_some() { "0" } else { "1" });
    let template = if let Some(app_id) = &client.app_id {
        params.insert_rc(
            "source",
            source
                .map_or_else(|| "native".to_owned(), |s| s.to_string())
                .into(),
        );
        params.insert("app_id", app_id.as_str());
        params.insert_rc(
            "icon",
            dash_frontend::game_icon_path(app_id)
                .unwrap_or_default()
                .into(),
        );
        params.insert("icon_builtin", "@/edit/3d.svg");
        "Client"
    } else {
        "ClientShow"
    };
    parser_state.instantiate_template(doc_params, template, layout, root, params)?;
    let button = parser_state.fetch_component_as::<ComponentButton>(&format!("client_{idx}"))?;
    button.set_sticky_state(&mut layout.common(), client.selected);
    Ok(())
}

/// Frame shell: the headset's own group (WayVR apps, local pins, the runtime's other clients) in `apps_root`, and
/// a group per PC in `hosts_root` with its screens, pins and running applications
fn rebuild_groups(
    app: &mut AppState,
    me: &mut OverlayList,
    layout: &mut Layout,
    parser_state: &mut ParserState,
    doc_params: &ParseDocumentParams,
) -> anyhow::Result<()> {
    let hosts_root = parser_state.get_widget_id("hosts_root").unwrap_or_default();
    let apps_root = parser_state.get_widget_id("apps_root").unwrap_or_default();
    let panels_root = parser_state
        .get_widget_id("panels_root")
        .unwrap_or_default();
    if hosts_root.is_null() || apps_root.is_null() {
        return Ok(());
    }
    layout.remove_children(hosts_root);

    let pins = app.session.config.pinned_games.clone();
    let mut client_idx = 0;

    // The headset: local pins, then running applications that no pin stands for
    for (i, pin) in pins.iter().enumerate() {
        if pin.host.is_none() {
            add_pin(app, layout, parser_state, doc_params, apps_root, i)?;
        }
    }
    for client in crate::frame_shell::native_clients(app) {
        let pinned = client.app_id.as_ref().is_some_and(|id| {
            pins.iter()
                .any(|pin| pin.host.is_none() && *pin.app_id == **id)
        });
        if !pinned {
            add_client(
                layout,
                parser_state,
                doc_params,
                apps_root,
                client_idx,
                None,
                &client,
                None,
            )?;
            client_idx += 1;
        }
    }

    // Empty parts are hidden, so they add no gaps: one separator between neighbours, spaced alike on both sides
    let mut native = false;
    for root in [apps_root, panels_root] {
        let mut children = Vec::new();
        layout.collect_children_ids_recursive(root, &mut children);
        layout
            .common()
            .alterables
            .set_widget_visible(root, !children.is_empty());
        native |= !children.is_empty();
    }

    let hosts = crate::frame_shell::top_row_hosts();
    layout
        .common()
        .alterables
        .set_widget_visible(hosts_root, !hosts.is_empty());
    for (g, host) in hosts.iter().enumerate() {
        let connected = host.source.is_some();
        let mut params = TemplateParams::new();
        params.insert_rc("idx", g.to_string().into());
        params.insert("name", host.name.as_str());
        params.insert(
            "presenting_display",
            if host.presenting { "flex" } else { "none" },
        );
        params.insert("separator", if g > 0 || native { "flex" } else { "none" });
        params.insert("status", host.status.as_str());
        params.insert("cookie", host.cookie.as_str());
        params.insert("connected_skip", if connected { "0" } else { "1" });
        let (action, label) = if connected {
            (
                format!("::FrameShell disconnect {}", host.cookie),
                "Disconnect",
            )
        } else {
            (format!("::FrameShell connect {}", host.cookie), "Connect")
        };
        params.insert_rc("action", action.into());
        params.insert("action_label", label);
        parser_state.instantiate_template(doc_params, "HostGroup", layout, hosts_root, params)?;
        let group = parser_state.get_widget_id(&format!("host_{g}"))?;
        parser_state
            .fetch_component_as::<ComponentButton>(&format!("hostbtn_{g}"))?
            .set_sticky_state(&mut layout.common(), connected);

        // Its screens
        for (i, meta) in me.last_metas.iter().enumerate() {
            if !matches!(meta.category, OverlayCategory::Screen) || !host.owns_screen(&meta.name) {
                continue;
            }
            let mut params = TemplateParams::new();
            params.insert(
                "keep_visible_display",
                if meta.keep_visible_when_hidden {
                    "flex"
                } else {
                    "none"
                },
            );
            params.insert_rc(
                "display",
                screen_label(
                    &crate::frame_hosts::monitor_label(&meta.name)
                        .unwrap_or_else(|| meta.name.to_string()),
                )
                .into(),
            );
            params.insert_rc("idx", i.to_string().into());
            params.insert("name", meta.name.as_ref());
            parser_state.instantiate_template(doc_params, "Screen", layout, group, params)?;
            let button =
                parser_state.fetch_component_as::<ComponentButton>(&format!("overlay_{i}"))?;
            button.set_sticky_state_immediate(&mut layout.common(), meta.visible);
            me.overlay_buttons.insert(meta.id, button);
        }

        // Its pins, which turn into the running game's button while it runs, then other running applications
        for (i, pin) in pins.iter().enumerate() {
            if pin.host.as_deref() == Some(host.cookie.as_str()) {
                add_pin(app, layout, parser_state, doc_params, group, i)?;
            }
        }
        for client in &host.clients {
            let pinned = client.app_id.as_ref().is_some_and(|id| {
                pins.iter().any(|pin| {
                    pin.host.as_deref() == Some(host.cookie.as_str()) && *pin.app_id == **id
                })
            });
            if !pinned {
                add_client(
                    layout,
                    parser_state,
                    doc_params,
                    group,
                    client_idx,
                    host.source,
                    client,
                    None,
                )?;
                client_idx += 1;
            }
        }
    }
    Ok(())
}

/// A screen's short label: its connector's first and last character, e.g. "D2" for DP-2
fn screen_label(name: &str) -> String {
    let output = name.rsplit_once(' ').map_or(name, |(_, output)| output);
    format!(
        "{}{}",
        output.chars().next().unwrap_or_default(),
        output.chars().last().unwrap_or_default()
    )
}
