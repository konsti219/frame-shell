//! PC monitors: their overlays, frames and the input sent back

use super::*;

pub(crate) extern "C" fn desktop_surface(info: *const DesktopSurface) {
    let Some(info) = (unsafe { info.as_ref() }) else {
        return;
    };
    let key = surface_key(info.source, info.surface);
    if info.removed {
        if let Some((frame, _)) = surfaces().remove(&key).and_then(|surface| surface.frame) {
            release_frame(frame.id);
        }
        return;
    }
    // Overlay names carry their saved layout, so name them after the PC and its monitor
    let text = frame_abi::c_string::<64>;
    let name: Arc<str> = format!("{} {}", text(&info.source_name), text(&info.name))
        .trim()
        .into();
    log::info!("Desktop {name}: {}x{}", info.width, info.height);
    surfaces().entry(key).or_insert(Surface {
        visible: None,
        name,
        connector: text(&info.name),
        registered: false,
        frame: None,
        spawned: 0,
    });
}

pub(crate) extern "C" fn desktop_frame(frame: *const DesktopFrame) {
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        return;
    };
    // The fd is borrowed for this call: keep our own
    let Ok(fd) = unsafe { BorrowedFd::borrow_raw(frame.fd) }.try_clone_to_owned() else {
        release_frame(frame.id);
        return;
    };
    let key = surface_key(frame.source, frame.surface);
    // A newer frame replaces one that wasn't shown yet
    let replaced = match surfaces().get_mut(&key) {
        Some(surface) => surface.frame.replace((*frame, fd)).map(|(old, _)| old.id),
        None => Some(frame.id),
    };
    if let Some(id) = replaced {
        release_frame(id);
    }
}

pub(crate) extern "C" fn metric(
    stream: *const std::ffi::c_char,
    stage: *const std::ffi::c_char,
    seq: u64,
    bytes: u64,
) {
    use wayvr_ipc::metrics;
    if metrics::listening() && !stream.is_null() && !stage.is_null() {
        let text = |text| unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
        metrics::emit(&text(stream), &text(stage), seq, bytes);
    }
}

/// Spawns an overlay for each announced monitor; vanished monitors keep theirs (blank) to keep its layout.
pub fn poll_desktop_visibility<T: Default>(
    overlays: &crate::windowing::manager::OverlayWindowManager<T>,
) {
    if !EMBEDDED.load(Ordering::Relaxed) {
        return;
    }
    for (&key, surface) in surfaces().iter_mut() {
        let visible = overlays.iter().any(|(_, overlay)| {
            overlay.config.name == surface.name
                && overlay
                    .config
                    .active_state
                    .as_ref()
                    .is_some_and(|state| state.alpha > 0.05)
        });
        if surface.visible != Some(visible) {
            surface.visible = Some(visible);
            let state = if visible { "visible" } else { "hidden" };
            wayvr_ipc::metrics::event(&surface.connector, state);
            crate::frame_host::send(
                (key >> 32) as u32,
                &wayvr_ipc::frame::ShellMessage::DesktopVisibility {
                    surface: key as u32,
                    visible,
                },
            );
        }
    }
}

pub fn poll_surfaces(app: &mut AppState) {
    if !EMBEDDED.load(Ordering::Relaxed) {
        return;
    }
    #[cfg(feature = "feat-monado-metrics")]
    {
        let frames = crate::frame_host::take_frames();
        if !CONNECTED.load(Ordering::Relaxed) {
            app.remote_metrics = None;
        } else if !frames.is_empty() {
            let (updated, metrics) = app
                .remote_metrics
                .get_or_insert_with(|| (std::time::Instant::now(), Default::default()));
            *updated = std::time::Instant::now();
            for frame in frames {
                metrics.ingest_remote(frame.into());
            }
        }
    }
    for error in crate::frame_host::take_errors() {
        toast(app, error);
    }
    for notification in crate::frame_host::take_notifications() {
        use wayvr_ipc::frame::NotificationSource;
        let topic = match notification.source {
            NotificationSource::Desktop => wlx_common::overlays::ToastTopic::DesktopNotification,
            NotificationSource::Overlay => wlx_common::overlays::ToastTopic::XSNotification,
        };
        let toast = crate::overlays::toast::Toast::new(
            topic,
            Some(wgui::i18n::Translation::from_raw_text_string(
                notification.title,
            )),
            wgui::i18n::Translation::from_raw_text_string(notification.body),
        )
        .with_timeout(notification.timeout_seconds)
        .with_sound(notification.sound);
        app.notifications.push(toast.build_raw());
    }
    if crate::frame_host::games_expired() {
        bump_lobby_state();
    }
    // Power key held for a second: power menu
    let down = POWER_KEY_DOWN_MS.load(Ordering::Relaxed);
    if down != 0
        && START.elapsed().as_millis() as u64 >= down + 1000
        && !POWER_KEY_HELD.swap(true, Ordering::Relaxed)
    {
        POWER_MENU_REQUEST.store(true, Ordering::Relaxed);
        SHOW_DASHBOARD.store(true, Ordering::Relaxed);
    }
    if let Some(percent) = VOLUME_TOAST.swap(0, Ordering::Relaxed).checked_sub(1) {
        toast(app, format!("Volume {percent} %"));
    }
    update_floor(app);
    let changed = LAYOUT_CHANGED_MS.load(Ordering::Relaxed);
    if changed != 0 && START.elapsed().as_millis() as u64 >= changed + 1000 {
        LAYOUT_CHANGED_MS.store(0, Ordering::Relaxed);
        app.tasks
            .enqueue(TaskType::Overlay(OverlayTask::SaveLayout));
    }
    if SHOW_DASHBOARD.swap(false, Ordering::Relaxed) {
        app.tasks
            .enqueue(TaskType::Overlay(OverlayTask::ShowDashboard));
    }
    // Start what was waiting for a connection
    let waiting = std::mem::take(&mut *PENDING_LAUNCH.lock().unwrap());
    for (host, app_id, name) in waiting {
        if let Some(source) = source_for_host(&host) {
            start_app_on(source, &app_id);
            let label = crate::frame_hosts::for_source(source).map_or(host, |pc| pc.name);
            toast(app, format!("Starting {name} on {label}"));
        } else {
            PENDING_LAUNCH
                .lock()
                .unwrap()
                .push_back((host, app_id, name));
        }
    }
    for (&key, surface) in surfaces()
        .iter_mut()
        .filter(|(_, surface)| !surface.registered)
    {
        if let Some(name) =
            crate::frame_hosts::monitor((key >> 32) as u32, &surface.connector)
        {
            surface.name = name.into();
            surface.registered = true;
        }
    }
    let run = RUN.load(Ordering::Relaxed);
    // Screens of earlier sessions come back as placeholders until their PC streams them again
    if SCREENS_SPAWNED_RUN.swap(run, Ordering::Relaxed) != run {
        for name in remembered_screens() {
            let name: Arc<str> = name.into();
            app.tasks.enqueue(TaskType::Overlay(OverlayTask::Spawn(
                OverlaySelector::Name(name.clone()),
                SpawnPos::Spread,
                Box::new(move |app| Some(desktop_overlay(name, 0, app))),
            )));
        }
    }
    for (&key, surface) in surfaces()
        .iter_mut()
        .filter(|(_, surface)| surface.registered && surface.spawned != run)
    {
        surface.spawned = run;
        let name = surface.name.clone();
        app.tasks.enqueue(TaskType::Overlay(OverlayTask::Spawn(
            OverlaySelector::Name(name.clone()),
            SpawnPos::Spread,
            Box::new(move |app| Some(desktop_overlay(name, key, app))),
        )));
    }
}

/// Forwards desktop input to the PC; pointer positions are normalized (0..1) on `POINTER_SURFACE`.
#[derive(Default)]
pub struct PcInput {
    modifiers: u8,
    held: BTreeSet<u16>,
    /// PC the pointer is on: buttons and keys go there
    source: u32,
}

impl PcInput {
    pub fn release(&mut self) {
        for code in std::mem::take(&mut self.held) {
            enqueue(ShellInput {
                kind: 0,
                code: code.into(),
                source: self.source,
                ..Default::default()
            });
        }
        self.modifiers = 0;
        enqueue(ShellInput {
            kind: 4,
            source: self.source,
            ..Default::default()
        });
    }
}

impl HidProvider for PcInput {
    fn mouse_move(&mut self, pos: DVec2) {
        let key = POINTER_SURFACE.load(Ordering::Relaxed);
        // Moving to another PC lets go of what is held on the previous one
        if (key >> 32) as u32 != self.source {
            PcInput::release(self);
            self.source = (key >> 32) as u32;
        }
        enqueue(ShellInput {
            kind: 2,
            x: pos.x.clamp(0., 1.) as f32,
            y: pos.y.clamp(0., 1.) as f32,
            source: (key >> 32) as u32,
            surface: key as u32,
            ..Default::default()
        });
    }
    fn mouse_move_rel(&mut self, _: DVec2) {}
    fn send_button(&mut self, button: u16, down: bool) {
        enqueue(ShellInput {
            kind: 1,
            code: button.saturating_sub(0x110).into(),
            x: f32::from(down),
            source: self.source,
            ..Default::default()
        });
    }
    fn wheel(&mut self, delta: WheelDelta) {
        enqueue(ShellInput {
            kind: 3,
            x: delta.x,
            y: delta.y,
            source: self.source,
            ..Default::default()
        });
    }
    fn set_desktop_extent(&mut self, _: DVec2) {}
    fn set_desktop_origin(&mut self, _: DVec2) {}
    fn set_modifiers(&mut self, mods: u8) {
        let changed = self.modifiers ^ mods;
        for bit in 0..8 {
            let mask = 1 << bit;
            if changed & mask != 0 {
                if let Some(key) = MODS_TO_KEYS.get(mask).and_then(|keys| keys.first()) {
                    self.send_key(*key, mods & mask != 0);
                }
            }
        }
        self.modifiers = mods;
    }
    fn send_key(&mut self, key: VirtualKey, down: bool) {
        let code = (key as u16).saturating_sub(8);
        if down {
            self.held.insert(code);
        } else {
            self.held.remove(&code);
        }
        enqueue(ShellInput {
            kind: 0,
            code: code.into(),
            x: f32::from(down),
            source: self.source,
            ..Default::default()
        });
    }
    fn set_keymap(&mut self, _: &XkbKeymap) {}
    fn commit(&mut self) {}
    fn release(&mut self) {
        PcInput::release(self);
    }
}

pub(crate) fn desktop_overlay(name: Arc<str>, key: u64, app: &AppState) -> OverlayWindowConfig {
    OverlayWindowConfig {
        name: name.clone(),
        category: OverlayCategory::Screen,
        show_on_spawn: true,
        input_focus: Some(InputFocus::PhysicalScreen),
        default_state: OverlayWindowState {
            positioning: app.session.config.default_positioning.into(),
            grabbable: true,
            interactable: true,
            transform: glam::Affine3A::from_scale_rotation_translation(
                glam::Vec3::splat(1.5),
                glam::Quat::IDENTITY,
                glam::vec3(0., 0., -1.),
            ),
            ..Default::default()
        },
        ..OverlayWindowConfig::from_backend(Box::new(DesktopOverlay {
            name: name.clone(),
            key,
            offline: None,
            redraw: false,
            meta: None,
            planes: None,
            shown: None,
            imports: HashMap::new(),
            renderer: None,
            last_hover: None,
        }))
    }
}

pub(crate) struct DesktopOverlay {
    name: Arc<str>,
    key: u64,
    /// Shown while the PC isn't streaming this screen, in the monitor's aspect, with the state it shows; a click
    /// connects
    offline: Option<(GuiPanel<()>, [String; 3])>,
    /// Redraw without a new frame: edit mode left its panel in the swapchain image
    redraw: bool,
    meta: Option<FrameMeta>,
    /// Luma and chroma views of the frame being shown
    planes: Option<(Arc<ImageView>, Arc<ImageView>)>,
    /// Decoder buffer of the frame being shown, released once a newer one is: WayVR waits for the previous
    /// frame's GPU work before rendering the next
    shown: Option<u64>,
    /// The decoder cycles through a fixed pool of DMA-bufs: import each once
    imports: HashMap<u64, (Arc<ImageView>, Arc<ImageView>)>,
    renderer: Option<Nv12ScreenPipeline>,
    /// Pointer and monitor pixel of the last hover, to leave the PC's mouse alone under a resting controller
    last_hover: Option<(usize, [i32; 2])>,
}

/// What a screen without a picture says: its state, why, and what a click does
fn screen_state(name: &str) -> [String; 3] {
    use crate::frame_hosts::Connection;
    use wayvr_ipc::frame::MonitorStatus;
    let Some(pc) = crate::frame_hosts::owner(name) else {
        return ["Unknown PC".into(), String::new(), String::new()];
    };
    let connect = || "Click to connect".to_owned();
    let line = |status: &str, detail: String| [status.to_owned(), detail, String::new()];
    let Some(source) = pc.source else {
        return match pc.connection {
            Connection::Connecting | Connection::Connected => line("Connecting…", String::new()),
            Connection::Pairing => line("Waiting for PIN", String::new()),
            Connection::Error => ["Connection failed".into(), pc.detail, connect()],
            _ if pc.available && !pc.compatible => line("Incompatible version", String::new()),
            _ if pc.available => ["Disconnected".into(), String::new(), connect()],
            _ => ["PC not found".into(), String::new(), connect()],
        };
    };
    let Some(capture) = crate::frame_host::capture(source) else {
        return line("Waiting for picture…", String::new());
    };
    if let Some(error) = capture.failed {
        return line("Capture failed", error);
    }
    let connector = crate::frame_hosts::monitor_label(name).unwrap_or_default();
    match capture.monitors.into_iter().find(|(known, _)| *known == connector) {
        None => line("Monitor not found", format!("{} has no {connector}", pc.name)),
        Some((_, MonitorStatus::Queued)) => line("Waiting for screen sharing", String::new()),
        Some((_, MonitorStatus::Dialog)) => line(
            &format!("Accept screen sharing on {}", pc.name),
            format!("Pick {connector} in the dialog"),
        ),
        Some((_, MonitorStatus::Streaming)) => line("Waiting for picture…", String::new()),
        Some((_, MonitorStatus::Failed(error))) => line("Capture failed", error),
    }
}

impl DesktopOverlay {
    /// The placeholder, rebuilt when what it says changes
    fn placeholder(&mut self, app: &mut AppState) -> anyhow::Result<ShouldRender> {
        let state = screen_state(&self.name);
        if self.offline.as_ref().is_none_or(|(_, shown)| *shown != state) {
            self.offline = Some((self.offline_panel(app, &state)?, state));
        }
        self.offline.as_mut().unwrap().0.should_render(app)
    }

    fn offline_panel(&self, app: &mut AppState, state: &[String; 3]) -> anyhow::Result<GuiPanel<()>> {
        // Keep the overlay's shape: the monitor's last aspect, 16:9 before any frame
        let [width, height] = self.meta.map_or([1920, 1080], |meta| meta.extent);
        let panel_w = 1280u32;
        let panel_h = (u64::from(panel_w) * u64::from(height) / u64::from(width.max(1))) as u32;
        let mut panel = GuiPanel::new_from_template(
            app,
            "gui/frame-screen-offline.xml",
            (),
            NewGuiPanelParams {
                extra_vars: [
                    ("name".into(), crate::frame_hosts::screen_title(&self.name).into()),
                    ("status".into(), state[0].clone().into()),
                    ("detail".into(), state[1].clone().into()),
                    ("hint".into(), state[2].clone().into()),
                    ("panel_w".into(), panel_w.to_string().into()),
                    ("panel_h".into(), panel_h.to_string().into()),
                ]
                .into(),
                ..Default::default()
            },
        )?;
        panel.update_layout(app)?;
        panel.init(app)?;
        Ok(panel)
    }
}

impl Drop for DesktopOverlay {
    fn drop(&mut self) {
        if let Some(id) = self.shown.take() {
            release_frame(id);
        }
    }
}

impl OverlayBackend for DesktopOverlay {
    fn init(&mut self, _: &mut AppState) -> anyhow::Result<()> {
        Ok(())
    }
    fn pause(&mut self, _: &mut AppState) -> anyhow::Result<()> {
        Ok(())
    }
    fn resume(&mut self, app: &mut AppState) -> anyhow::Result<()> {
        self.redraw = true;
        match &mut self.offline {
            Some((offline, _)) => offline.resume(app),
            None => Ok(()),
        }
    }
    fn should_render(&mut self, app: &mut AppState) -> anyhow::Result<ShouldRender> {
        // Bound by name: the key of a monitor can differ between sessions, and remembered screens start without one
        let latest = match surfaces()
            .iter_mut()
            .find(|(_, surface)| surface.name == self.name)
        {
            Some((&key, surface)) => {
                self.key = key;
                let frame = surface.frame.take();
                if let Some((frame, _)) = &frame {
                    wayvr_ipc::metrics::emit(&surface.connector, "shown", frame.id, 0);
                }
                frame
            }
            None => {
                self.planes = None;
                if let Some(id) = self.shown.take() {
                    release_frame(id);
                }
                self.imports.clear();
                self.renderer = None;
                return self.placeholder(app);
            }
        };
        if latest.is_some() {
            self.offline = None;
        } else if self.offline.is_some() || self.planes.is_none() {
            // Connected, first frame still on its way: keep the placeholder (a fresh screen has nothing else)
            return self.placeholder(app);
        }
        if let Some((frame, fd)) = latest {
            let extent = [frame.width, frame.height];
            if self.meta.is_some_and(|old| old.extent != extent) || self.imports.len() > 32 {
                self.imports.clear();
            }
            if !self.imports.contains_key(&frame.id) {
                let plane =
                    |format, size: [u32; 2], offset: u64| -> anyhow::Result<Arc<ImageView>> {
                        let image = app.gfx.import_dmabuf(&DmabufImportInfo {
                            extent: [size[0], size[1], 1],
                            format,
                            fd: fd.as_raw_fd(),
                            modifier: Some(0),
                            plane_layouts: vec![DmabufPlaneLayout {
                                offset,
                                row_pitch: frame.stride.into(),
                            }],
                        })?;
                        app.gfx.create_image_view(image)
                    };
                let planes = plane(Format::R8_UNORM, extent, 0).and_then(|luma| {
                    let chroma = plane(
                        Format::R8G8_UNORM,
                        [frame.width.div_ceil(2), frame.height.div_ceil(2)],
                        frame.chroma_offset,
                    )?;
                    Ok((luma, chroma))
                });
                // A frame that didn't import goes back to the decoder, which has only a few
                let planes = planes.inspect_err(|_| release_frame(frame.id))?;
                self.imports.insert(frame.id, planes);
            }
            self.planes = self.imports.get(&frame.id).cloned();
            if let Some(old) = self.shown.replace(frame.id)
                && old != frame.id
            {
                release_frame(old);
            }
            let meta = FrameMeta {
                extent,
                ..Default::default()
            };
            // Also after a disconnect, which drops the renderer but keeps the size for the placeholder
            if self.renderer.is_none() || self.meta.is_none_or(|old| old.extent != extent) {
                self.renderer = Some(Nv12ScreenPipeline::new(&meta, app)?);
            }
            self.meta = Some(meta);
            Ok(ShouldRender::Should)
        } else {
            Ok(match (self.planes.is_some(), std::mem::take(&mut self.redraw)) {
                (true, true) => ShouldRender::Should,
                (true, false) => ShouldRender::Can,
                (false, _) => ShouldRender::Unable,
            })
        }
    }
    fn render(
        &mut self,
        app: &mut AppState,
        resources: &mut RenderResources,
    ) -> anyhow::Result<()> {
        if let Some((offline, _)) = &mut self.offline {
            return offline.render(app, resources);
        }
        let (luma, chroma) = self.planes.clone().unwrap();
        self.renderer
            .as_mut()
            .unwrap()
            .render(luma, chroma, app, resources)
    }
    fn frame_meta(&mut self) -> Option<FrameMeta> {
        match &mut self.offline {
            Some((offline, _)) => offline.frame_meta(),
            None => self.meta,
        }
    }
    fn notify(&mut self, _: &mut AppState, _: OverlayEventData) -> anyhow::Result<()> {
        Ok(())
    }
    // WayVR's own screen input (hand priority, move throttle, click freeze, left-handed mode, scrolling) on
    // this surface's normalized coordinates
    fn on_hover(&mut self, app: &mut AppState, hit: &PointerHit) -> HoverResult {
        if let Some((offline, _)) = &mut self.offline {
            return offline.on_hover(app, hit);
        }
        POINTER_SURFACE.store(self.key, Ordering::Relaxed);
        // A pointer that didn't move doesn't move the mouse, so the PC's own mouse keeps working
        if let Some(meta) = self.meta {
            let pixel = [
                (hit.uv.x * meta.extent[0] as f32).round() as i32,
                (hit.uv.y * meta.extent[1] as f32).round() as i32,
            ];
            if self.last_hover.replace((hit.pointer, pixel)) == Some((hit.pointer, pixel)) {
                return HoverResult::consume();
            }
        }
        screen_hover(app, hit, hit.uv)
    }
    fn on_left(&mut self, app: &mut AppState, pointer: usize) {
        if let Some((offline, _)) = &mut self.offline {
            offline.on_left(app, pointer);
        }
    }
    fn on_pointer(&mut self, app: &mut AppState, hit: &PointerHit, pressed: bool) {
        if self.offline.is_some() {
            // Only ever connects: another screen of the same PC may already be streaming
            if pressed {
                connect_screen_host(app, &self.name);
            }
            return;
        }
        POINTER_SURFACE.store(self.key, Ordering::Relaxed);
        screen_pointer(app, hit, pressed, hit.uv);
    }
    fn on_scroll(&mut self, app: &mut AppState, _: &PointerHit, delta: WheelDelta) {
        if self.offline.is_none() {
            screen_scroll(app, delta);
        }
    }
    fn get_interaction_transform(&mut self) -> Option<Affine2> {
        match &mut self.offline {
            Some((offline, _)) => offline.get_interaction_transform(),
            None => self.meta.map(|meta| ui_transform(meta.extent)),
        }
    }
    fn get_attrib(&self, _: BackendAttrib) -> Option<BackendAttribValue> {
        None
    }
    fn set_attrib(&mut self, _: &mut AppState, _: BackendAttribValue) -> bool {
        false
    }
}
