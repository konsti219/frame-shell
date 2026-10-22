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

            let mut stop_clicks_to_reset = Vec::<String>::new();

            for (i, pin) in app.session.config.pinned_games.iter().enumerate() {
                // In the frame shell, pins go into their PC's group (rebuild_groups)
                if crate::frame_shell::EMBEDDED.load(std::sync::atomic::Ordering::Relaxed) {
                    continue;
                }
                if !add_pin(app, layout, parser_state, doc_params, apps_root, i)? {
                    stop_clicks_to_reset.push(crate::frame_shell::pinned_stop_key(
                        pin.host.as_deref(),
                        &pin.app_id,
                    ));
                }
            }

            for pin_id in stop_clicks_to_reset {
                app.session.pinned_stop_clicks.remove(&pin_id);
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
                    OverlayCategory::Screen if crate::frame_shell::EMBEDDED.load(std::sync::atomic::Ordering::Relaxed) => continue,
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
                let keyboard_id = self
                    .last_metas
                    .iter()
                    .find(|meta| matches!(meta.category, OverlayCategory::Keyboard))
                    .map(|meta| meta.id);
                if let Some(keyboard_id) = keyboard_id
                    && !overlays.as_ref().contains(&keyboard_id)
                {
                    // Closing the keyboard resets the stop click sequence.
                    app.session.pinned_stop_clicks.clear();
                }

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

    let stop_stage = app
        .session
        .pinned_stop_clicks
        .get(&crate::frame_shell::pinned_stop_key(
            pin.host.as_deref(),
            &pin.app_id,
        ))
        .copied()
        .unwrap_or(0);

    let mut params = TemplateParams::new();
    params.insert_rc("idx", i.to_string().into());
    params.insert("name", pin.name.as_str());
    params.insert_rc(
        "icon",
        if running && stop_stage >= 1 {
            "".into()
        } else {
            pin.icon
                .as_ref()
                .map_or_else(|| "".into(), |x| x.as_str().into())
        },
    );
    params.insert(
        "icon_builtin",
        if running && stop_stage >= 2 {
            "@/dashboard/knife.svg"
        } else if running && stop_stage >= 1 {
            "@/dashboard/remove_circle.svg"
        } else {
            "@/edit/panel.svg"
        },
    );

    parser_state.instantiate_template(doc_params, "PinnedApp", layout, root, params)?;

    let pin_button = parser_state.fetch_component_as::<ComponentButton>(&format!("pinned_{i}"))?;
    if running {
        pin_button.set_sticky_state(&mut layout.common(), true);
    }
    Ok(running)
}

/// A running application's button in `root`: left shows it, right stops Steam games
fn add_client(
    layout: &mut Layout,
    parser_state: &mut ParserState,
    doc_params: &ParseDocumentParams,
    root: wgui::layout::WidgetID,
    idx: usize,
    source: Option<u32>,
    client: &crate::frame_shell::RunningClient,
) -> anyhow::Result<()> {
    let mut params = TemplateParams::new();
    params.insert_rc("idx", idx.to_string().into());
    params.insert("name", client.name.as_str());
    params.insert("show", client.show.as_str());
    let template = if let Some(app_id) = &client.app_id {
        params.insert_rc(
            "source",
            source.map_or_else(|| "native".to_owned(), |s| s.to_string()).into(),
        );
        params.insert("app_id", app_id.as_str());
        params.insert_rc(
            "icon",
            dash_frontend::game_icon_path(app_id).unwrap_or_default().into(),
        );
        params.insert("icon_builtin", "@/edit/3d.svg");
        "Client"
    } else {
        "ClientShow"
    };
    parser_state.instantiate_template(doc_params, template, layout, root, params)?;
    let button = parser_state.fetch_component_as::<ComponentButton>(&format!("client_{idx}"))?;
    button.set_sticky_state(&mut layout.common(), true);
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
    let panels_root = parser_state.get_widget_id("panels_root").unwrap_or_default();
    if hosts_root.is_null() || apps_root.is_null() {
        return Ok(());
    }
    layout.remove_children(hosts_root);

    let pins = app.session.config.pinned_games.clone();
    let mut client_idx = 0;

    // The headset: local pins, then running applications that no pin stands for
    for (i, pin) in pins.iter().enumerate() {
        if pin.host.is_none() && !add_pin(app, layout, parser_state, doc_params, apps_root, i)? {
            app.session
                .pinned_stop_clicks
                .remove(&crate::frame_shell::pinned_stop_key(None, &pin.app_id));
        }
    }
    for client in crate::frame_shell::native_clients(app) {
        let pinned = client.app_id.as_ref().is_some_and(|id| {
            pins.iter().any(|pin| pin.host.is_none() && *pin.app_id == **id)
        });
        if !pinned {
            add_client(layout, parser_state, doc_params, apps_root, client_idx, None, &client)?;
            client_idx += 1;
        }
    }

    // Empty parts are hidden, so they add no gaps: one separator between neighbours, spaced alike on both sides
    let mut native = false;
    for root in [apps_root, panels_root] {
        let mut children = Vec::new();
        layout.collect_children_ids_recursive(root, &mut children);
        layout.common().alterables.set_widget_visible(root, !children.is_empty());
        native |= !children.is_empty();
    }

    // PCs: known ones, then any that only remembered screens or pins name
    let mut hosts = crate::frame_shell::top_row_hosts();
    let mut extra = Vec::<String>::new();
    for meta in me.last_metas.iter() {
        if matches!(meta.category, OverlayCategory::Screen)
            && let Some((prefix, _)) = meta.name.rsplit_once(' ')
            && !hosts.iter().any(|h| h.screen_prefix == prefix)
            && !extra.iter().any(|e| e == prefix)
        {
            extra.push(prefix.to_owned());
        }
    }
    for pin in &pins {
        if let Some(host) = &pin.host
            && !hosts.iter().any(|h| h.name == *host)
            && !extra.iter().any(|e| e == host)
        {
            extra.push(host.clone());
        }
    }
    for name in extra {
        hosts.push(crate::frame_shell::TopRowHost {
            screen_prefix: name.clone(),
            name,
            cookie: String::new(),
            source: None,
            presenting: false,
            clients: Vec::new(),
        });
    }

    layout.common().alterables.set_widget_visible(hosts_root, !hosts.is_empty());
    for (g, host) in hosts.iter().enumerate() {
        let connected = host.source.is_some();
        let mut params = TemplateParams::new();
        params.insert_rc("idx", g.to_string().into());
        params.insert("name", host.name.as_str());
        params.insert("presenting_display", if host.presenting { "flex" } else { "none" });
        params.insert("separator", if g > 0 || native { "flex" } else { "none" });
        let (action, label) = if host.cookie.is_empty() {
            (format!("::FrameShellScreen {} ", host.screen_prefix), "Connect / disconnect")
        } else if !connected {
            (format!("::FrameShell connect {}", host.cookie), "Connect")
        } else if host.presenting {
            ("::FrameShell disconnect".to_owned(), "Disconnect")
        } else {
            (format!("::FrameShell disconnect {}", host.cookie), "Disconnect")
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
            if !matches!(meta.category, OverlayCategory::Screen)
                || meta.name.rsplit_once(' ').map(|(prefix, _)| prefix) != Some(host.screen_prefix.as_str())
            {
                continue;
            }
            let mut params = TemplateParams::new();
            params.insert(
                "keep_visible_display",
                if meta.keep_visible_when_hidden { "flex" } else { "none" },
            );
            params.insert_rc("display", screen_label(&meta.name).into());
            params.insert_rc("idx", i.to_string().into());
            params.insert("name", meta.name.as_ref());
            parser_state.instantiate_template(doc_params, "Screen", layout, group, params)?;
            let button = parser_state.fetch_component_as::<ComponentButton>(&format!("overlay_{i}"))?;
            button.set_sticky_state_immediate(&mut layout.common(), meta.visible);
            me.overlay_buttons.insert(meta.id, button);
        }

        // Its pins, which turn into the running game's button while it runs, then other running applications
        for (i, pin) in pins.iter().enumerate() {
            if pin.host.as_deref() == Some(host.name.as_str())
                && !add_pin(app, layout, parser_state, doc_params, group, i)?
            {
                app.session.pinned_stop_clicks.remove(&crate::frame_shell::pinned_stop_key(
                    pin.host.as_deref(),
                    &pin.app_id,
                ));
            }
        }
        for client in &host.clients {
            let pinned = client.app_id.as_ref().is_some_and(|id| {
                pins.iter()
                    .any(|pin| pin.host.as_deref() == Some(host.name.as_str()) && *pin.app_id == **id)
            });
            if !pinned {
                add_client(layout, parser_state, doc_params, group, client_idx, host.source, client)?;
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
