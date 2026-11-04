//! Debug builds only: the commands `crate::dev_control` receives, each run
//! through the method its click or key would call. Never activates the
//! window: checking a change must not take the user's screen.

use gpui::{Context, Window};
use serde_json::json;

use super::KeelWindow;
use super::composer_view::Picker;

/// `trace on`: one row per frame of the layout the chat column gets (the
/// values render chose, and the bounds the last paint measured), so a
/// slide can be checked for rewraps and pops by numbers, not by eye.
#[derive(Default)]
pub(super) struct GeoTrace {
    pub on: std::cell::Cell<bool>,
    pub start: std::cell::Cell<Option<std::time::Instant>>,
    pub frames: std::cell::RefCell<Vec<serde_json::Value>>,
    /// Painted (x, width) of the transcript's list and the composer's box.
    pub list: std::cell::Cell<(f32, f32, f32)>,
    pub dock: std::cell::Cell<(f32, f32, f32)>,
    /// Session card: painted (x, width) of the full card and the pill, and
    /// this frame's (k, full opacity, pill opacity).
    pub card: std::cell::Cell<(f32, f32, f32)>,
    pub pill: std::cell::Cell<(f32, f32, f32)>,
    pub card_look: std::cell::Cell<(f32, f32, f32)>,
    /// Painted height of the todo/plan card under the session card (0 if absent).
    pub todo: std::cell::Cell<(f32, f32, f32)>,
    /// (steps_pending as 0/1, has_held_steps as 0/1, git_kind: 0 Unknown / 1 NoRepo / 2 Repo).
    pub card_meta: std::cell::Cell<(f32, f32, f32)>,
    /// Painted (x, width, height) of the composer panel and its send button.
    pub panel: std::cell::Cell<(f32, f32, f32)>,
    pub send: std::cell::Cell<(f32, f32, f32)>,
    /// Painted box of the outline (points) pill, and the last code block and
    /// tool row the transcript painted.
    pub points: std::cell::Cell<(f32, f32, f32)>,
    pub code: std::cell::Cell<(f32, f32, f32)>,
    pub tool: std::cell::Cell<(f32, f32, f32)>,
}

impl GeoTrace {
    /// A zero-size canvas recording its parent's painted (x, width) into `cell`.
    pub fn probe(
        this: &std::rc::Rc<Self>,
        which: fn(&Self) -> &std::cell::Cell<(f32, f32, f32)>,
    ) -> Option<gpui::AnyElement> {
        use gpui::{IntoElement, Styled};
        if !this.on.get() {
            return None;
        }
        let me = this.clone();
        Some(
            gpui::canvas(
                move |b, _, _| {
                    which(&me).set((
                        f32::from(b.origin.x),
                        f32::from(b.size.width),
                        f32::from(b.size.height),
                    ))
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0()
            .into_any_element(),
        )
    }

    pub fn push(&self, row: serde_json::Value) {
        if !self.on.get() {
            return;
        }
        let ms = self
            .start
            .get()
            .map_or(0., |at| at.elapsed().as_secs_f32() * 1e3);
        let (lx, lw, lh) = self.list.get();
        let (dx, dw, dh) = self.dock.get();
        let (cx, cw, ch) = self.card.get();
        let (px, pw, _) = self.pill.get();
        let (k, fo, po) = self.card_look.get();
        let (_, _, todo_h) = self.todo.get();
        let (pending, held, git_k) = self.card_meta.get();
        let (pnx, pnw, pnh) = self.panel.get();
        let (sx, sw, _) = self.send.get();
        let (ox, ow, _) = self.points.get();
        let (cdx, cdw, cdh) = self.code.get();
        let (tx, tw, th) = self.tool.get();
        // Reset so a layer that isn't drawn reads as absent next frame.
        self.card.set((0., 0., 0.));
        self.pill.set((0., 0., 0.));
        self.todo.set((0., 0., 0.));
        let mut frames = self.frames.borrow_mut();
        if frames.len() < 2000 {
            frames.push(json!({
                "ms": (ms * 10.).round() / 10.,
                "now": row,
                "painted": {
                    "list_x": lx, "list_w": lw, "dock_x": dx, "dock_w": dw,
                    "list_h": lh, "dock_h": dh,
                    "card_x": cx, "card_w": cw, "card_h": ch,
                    "pill_x": px, "pill_w": pw,
                    "todo_h": todo_h,
                    "panel_x": pnx, "panel_w": pnw, "panel_h": pnh,
                    "send_x": sx, "send_w": sw,
                    "points_x": ox, "points_w": ow,
                    "code_x": cdx, "code_w": cdw, "code_h": cdh,
                    "tool_x": tx, "tool_w": tw, "tool_h": th,
                },
                "card": {
                    "k": k, "full_op": fo, "pill_op": po,
                    "steps_pending": pending, "held_steps": held, "git_k": git_k,
                },
            }));
        }
    }
}

impl KeelWindow {
    pub(super) fn dev_command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> String {
        let (cmd, arg) = line
            .split_once(' ')
            .map_or((line, ""), |(c, a)| (c, a.trim()));
        let mut files_out = None;
        let answer = match cmd {
            "state" => return self.dev_state(cx),
            c if c.starts_with("files_") => {
                // Through the redraw below, so a screenshot shows the change.
                files_out = self.files_dev(c, arg, window, cx);
                if files_out.is_some() {
                    ""
                } else {
                    "unknown files command"
                }
            }
            "chats" => {
                let mut chats: Vec<_> = self
                    .state
                    .chats
                    .values()
                    .filter(|c| !c.archived && !c.tombstoned)
                    .collect();
                chats.sort_by_key(|c| std::cmp::Reverse(c.last_message_at));
                let rows: Vec<_> = chats
                    .iter()
                    .map(|c| json!({"id": c.id, "title": c.title, "space": c.space_id, "harness": c.harness, "model": c.model, "status": format!("{:?}", c.status)}))
                    .collect();
                return json!(rows).to_string();
            }
            "open" => {
                if !self.state.chats.contains_key(arg) {
                    return format!("error: no chat {arg:?}");
                }
                self.close_pages();
                self.finish_settings_exit(cx);
                self.state.open_or_focus_tab(arg);
                "ok"
            }
            "new" => {
                self.close_pages();
                self.create_new_session(None, cx);
                "ok"
            }
            "type" => {
                let text = arg.replace("\\n", "\n");
                self.composer.update(cx, |s, cx| {
                    s.set_value(&text, window, cx);
                    let end = s.value().len();
                    s.set_selected_range(end..end, cx);
                });
                self.update_mention(cx);
                "ok"
            }
            "send" => {
                self.handle_primary_button(window, cx);
                "ok"
            }
            "model" => {
                let known = self
                    .models
                    .get(&self.selected_harness)
                    .is_some_and(|l| l.models.iter().any(|m| m.id == arg));
                if !known {
                    return format!(
                        "error: {arg:?} is not a {} model (open `picker model` to load the list)",
                        self.selected_harness
                    );
                }
                self.select_model(arg.to_string(), cx);
                "ok"
            }
            "picker" => {
                if arg == "card_branch" {
                    self.card_open_branches(window, cx);
                    return "ok".into();
                }
                let picker = match arg {
                    "model" => Some(Picker::Model),
                    "branch" => Some(Picker::Branch),
                    "folder" => Some(Picker::Folder),
                    "repo" => Some(Picker::Repo),
                    "none" | "" => None,
                    other => return format!("error: no picker {other:?}"),
                };
                self.set_picker(picker, cx);
                "ok"
            }
            "settings" => {
                self.toggle_settings(cx);
                "ok"
            }
            "scroll_settings" => {
                let y: f32 = arg.parse().unwrap_or(-320.);
                self.dotify
                    .settings_scroll
                    .set_offset(gpui::point(gpui::px(0.), gpui::px(y)));
                "ok"
            }
            "sound_menu" => {
                self.show_sound_menu = !self.show_sound_menu;
                "ok"
            }
            "preview_sound" => {
                self.preview_sound(cx);
                "ok"
            }
            "skills" => {
                if !self.settings_open {
                    self.open_settings(cx);
                }
                if !self.skills.open {
                    self.open_skills(cx);
                }
                if !arg.is_empty() {
                    self.dev_toggle_skill_pack(arg, cx);
                }
                "ok"
            }
            "sidebar" => {
                self.toggle_sidebar(cx);
                "ok"
            }
            "browser" => {
                self.toggle_browser(window, cx);
                "ok"
            }
            "expand" | "browser_expand" => {
                self.toggle_browser_expand(cx);
                "ok"
            }
            "nav" | "browser_nav" => {
                if !self.browser.open {
                    self.toggle_browser(window, cx);
                }
                let url = crate::browser::normalize_address(arg).unwrap_or_else(|| arg.to_string());
                self.browser_load(url, window, cx);
                "ok"
            }
            "nav_hidden" => {
                let url = crate::browser::normalize_address(arg).unwrap_or_else(|| arg.to_string());
                self.browser_load(url, window, cx);
                "ok"
            }
            "wait_settle" => {
                let ms: u64 = arg.parse().unwrap_or(2000);
                let start = std::time::Instant::now();
                let active = self.browser.active;
                let _ = self.ensure_native_page(active, window, cx);
                if let Some(native) = self.browser.active_native_page() {
                    while start.elapsed() < std::time::Duration::from_millis(ms) {
                        native.spin();
                        #[cfg(target_os = "macos")]
                        crate::dev_control::redraw();
                        std::thread::sleep(std::time::Duration::from_millis(16));
                    }
                }
                "ok"
            }
            "shot" | "browser_shot" => {
                let path = if arg.is_empty() {
                    "/tmp/keel-cef-shot.png"
                } else {
                    arg
                };
                let path = std::path::PathBuf::from(path);
                let active = self.browser.active;
                let _ = self.ensure_native_page(active, window, cx);
                if let Some(native) = self.browser.active_native_page() {
                    match native.read_frame_png() {
                        Ok(bytes) => {
                            if std::fs::write(&path, bytes).is_ok() {
                                cx.notify();
                                #[cfg(target_os = "macos")]
                                crate::dev_control::redraw();
                                return format!("saved: {}", path.display());
                            }
                        }
                        Err(e) => return format!("error: {e}"),
                    }
                }
                "error: no active browser tab".into()
            }
            "mcp_snapshot" => {
                let mode = if arg == "text" { "text" } else { "interactive" };
                let active = self.browser.active;
                let _ = self.ensure_native_page(active, window, cx);
                if let Some(native) = self.browser.active_native_page() {
                    match native
                        .eval_agent_sync(&format!("snapshot({{ mode: {:?} }})", mode), 10000)
                    {
                        Ok(snap) => return snap,
                        Err(e) => return format!("error: {e}"),
                    }
                }
                "error: no active browser tab".into()
            }
            "mcp_click" => {
                let active = self.browser.active;
                let _ = self.ensure_native_page(active, window, cx);
                if let Some(native) = self.browser.active_native_page() {
                    let code = if arg.starts_with('{') {
                        format!("act('click', {})", arg)
                    } else {
                        format!("act('click', {{ ref: {:?} }})", arg)
                    };
                    match native.eval_agent_sync(&code, 10000) {
                        Ok(res) => {
                            cx.notify();
                            #[cfg(target_os = "macos")]
                            crate::dev_control::redraw();
                            return res;
                        }
                        Err(e) => return format!("error: {e}"),
                    }
                }
                "error: no active browser tab".into()
            }
            "mcp_type" => {
                let active = self.browser.active;
                let _ = self.ensure_native_page(active, window, cx);
                if let Some(native) = self.browser.active_native_page() {
                    let mut parts = arg.splitn(2, ' ');
                    let target_ref = parts.next().unwrap_or("");
                    let text = parts.next().unwrap_or("");
                    let code =
                        format!("act('type', {{ ref: {:?}, text: {:?} }})", target_ref, text);
                    match native.eval_agent_sync(&code, 10000) {
                        Ok(res) => {
                            cx.notify();
                            #[cfg(target_os = "macos")]
                            crate::dev_control::redraw();
                            return res;
                        }
                        Err(e) => return format!("error: {e}"),
                    }
                }
                "error: no active browser tab".into()
            }
            "mcp_scroll" => {
                let active = self.browser.active;
                let _ = self.ensure_native_page(active, window, cx);
                if let Some(native) = self.browser.active_native_page() {
                    let dy: f64 = arg.parse().unwrap_or(300.0);
                    let code = format!("act('scroll', {{ dy: {} }})", dy);
                    match native.eval_agent_sync(&code, 10000) {
                        Ok(res) => {
                            cx.notify();
                            #[cfg(target_os = "macos")]
                            crate::dev_control::redraw();
                            return res;
                        }
                        Err(e) => return format!("error: {e}"),
                    }
                }
                "error: no active browser tab".into()
            }
            "mcp_screenshot" => {
                let mut parts = arg.split_whitespace();
                let path = parts.next().unwrap_or("/tmp/keel-mcp-shot.png");
                let full = parts
                    .next()
                    .map(|p| p == "full" || p == "true")
                    .unwrap_or(false);
                let path = std::path::PathBuf::from(path);
                let active = self.browser.active;
                let _ = self.ensure_native_page(active, window, cx);
                if let Some(native) = self.browser.active_native_page() {
                    match native.screenshot_sync(full, 15000) {
                        Ok(bytes) => {
                            if std::fs::write(&path, &bytes).is_ok() {
                                return format!(
                                    "saved: {} ({} bytes)",
                                    path.display(),
                                    bytes.len()
                                );
                            } else {
                                return format!("error: failed to write to {}", path.display());
                            }
                        }
                        Err(e) => return format!("error: {e}"),
                    }
                }
                "error: no active browser tab".into()
            }
            "cdp" => {
                let active = self.browser.active;
                let _ = self.ensure_native_page(active, window, cx);
                if let Some(native) = self.browser.active_native_page() {
                    let mut parts = arg.splitn(2, ' ');
                    let method = parts.next().unwrap_or("");
                    let params: serde_json::Value = parts
                        .next()
                        .and_then(|p| serde_json::from_str(p).ok())
                        .unwrap_or(serde_json::json!({}));
                    match native.send_cdp_sync(method, params, 10000) {
                        Ok(res) => return res.to_string(),
                        Err(e) => return format!("error: {e}"),
                    }
                }
                "error: no active browser tab".into()
            }
            "mcp_eval" => {
                let active = self.browser.active;
                let _ = self.ensure_native_page(active, window, cx);
                if let Some(native) = self.browser.active_native_page() {
                    match native.send_cdp_sync(
                        "Runtime.evaluate",
                        serde_json::json!({
                            "expression": arg,
                            "returnByValue": true,
                            "awaitPromise": true
                        }),
                        10000,
                    ) {
                        Ok(res) => return res.to_string(),
                        Err(e) => return format!("error: {e}"),
                    }
                }
                "error: no active browser tab".into()
            }
            "browser_state" => {
                let b = &self.browser;
                let tab = b.tab();
                let native_info = b
                    .active_native_page()
                    .map(|n| n.debug_info())
                    .unwrap_or(serde_json::Value::Null);
                return json!({
                    "open": b.open,
                    "expanded": b.expanded,
                    "expanding": self.browser_expanding(),
                    "expand_from": b.expand_from,
                    "expand_fraction": self.browser_expanded_fraction(),
                    "active": b.active,
                    "tabs_count": b.tabs.len(),
                    "tab_id": tab.id,
                    "tab_page_url": tab.page.url,
                    "tab_page_title": tab.page.title,
                    "tab_loading": tab.page.loading,
                    "tab_progress": tab.page.progress,
                    "tab_parked": tab.parked,
                    "has_native": tab.native.is_some(),
                    "native": native_info,
                })
                .to_string();
            }
            "files" => {
                self.toggle_files(window, cx);
                "ok"
            }
            "show_file" => {
                self.show_file(arg.to_string(), window, cx);
                "ok"
            }
            "terminal" => {
                self.toggle_terminal(window, cx);
                "ok"
            }
            "suggest" => {
                if arg.is_empty() {
                    self.prompt_suggestion = None;
                } else {
                    self.prompt_suggestion = Some(arg.to_string());
                    self.prompt_suggestion_at = Some(std::time::Instant::now());
                }
                cx.notify();
                "ok"
            }
            "points" => {
                self.show_points_menu = !self.show_points_menu;
                cx.notify();
                "ok"
            }
            "effort" => {
                self.show_effort_menu = !self.show_effort_menu;
                cx.notify();
                "ok"
            }
            "permission" => {
                self.show_permission_menu = !self.show_permission_menu;
                cx.notify();
                "ok"
            }
            "layout" => {
                let (room_l, room_r) = self.card_column_room();
                let col = self.column_max();
                let open = self.sidebar_open_now();
                let right_open = self.right_panel_open_now();
                let collapsed = self.state.ui_settings.sidebar_collapsed;
                let (w, _) = self.session_card.size.get();
                return json!({
                    "sidebar_collapsed": collapsed,
                    "sidebar_open_now": open,
                    "right_panel_open_now": right_open,
                    "browser_open": self.browser.open,
                    "files_open": self.files.open,
                    "terminal_open": self.terminal.open,
                    "browser_allowed": self.browser_allowed(),
                    "column_max": f32::from(col),
                    "room_l": room_l,
                    "room_r": room_r,
                    "transcript_w": w,
                })
                .to_string();
            }
            "branch_bounds" => {
                let b = self.card_branch_bounds.get();
                return json!({
                    "bounds": b.map(|b| {
                        json!({
                            "left": f32::from(b.left()),
                            "top": f32::from(b.top()),
                            "right": f32::from(b.right()),
                            "bottom": f32::from(b.bottom()),
                            "width": f32::from(b.size.width),
                            "height": f32::from(b.size.height),
                        })
                    }),
                    "picking": self.session_card.picking,
                    "compact": self.session_card.compact,
                })
                .to_string();
            }
            "scroll" => {
                // Like the wheel: positive moves back up the history.
                let Ok(dy) = arg.parse::<f32>() else {
                    return format!("error: scroll takes pixels, got {arg:?}");
                };
                // The list moves first: `on_wheel` reads the post-scroll position.
                self.transcript.list_state.scroll_by(gpui::px(-dy));
                self.transcript.on_wheel(gpui::px(dy));
                "ok"
            }
            "trace" => {
                let tr = &self.geo_trace;
                match arg {
                    "on" => {
                        tr.frames.borrow_mut().clear();
                        tr.start.set(Some(std::time::Instant::now()));
                        tr.on.set(true);
                    }
                    "off" => tr.on.set(false),
                    _ => return json!(*tr.frames.borrow()).to_string(),
                }
                "ok"
            }
            c if c.starts_with("project_") => {
                let out = self.project_dev(c, arg, window, cx);
                if out != "ok" {
                    return out;
                }
                "ok"
            }
            "redraw" => "ok",
            "win_shot" => {
                let path = if arg.is_empty() {
                    "/tmp/keel-win-shot.png"
                } else {
                    arg
                };
                #[cfg(target_os = "macos")]
                {
                    #[repr(C)]
                    struct CGImage {
                        _private: [u8; 0],
                    }
                    unsafe impl objc2::Encode for CGImage {
                        const ENCODING: objc2::Encoding = objc2::Encoding::Struct("CGImage", &[]);
                    }
                    unsafe impl objc2::RefEncode for CGImage {
                        const ENCODING_REF: objc2::Encoding =
                            objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGImage", &[]));
                    }
                    #[link(name = "CoreGraphics", kind = "framework")]
                    unsafe extern "C" {
                        fn CGWindowListCreateImage(
                            screenBounds: objc2_foundation::NSRect,
                            listOption: u32,
                            windowID: u32,
                            imageOption: u32,
                        ) -> *const CGImage;
                        fn CGImageRelease(image: *const CGImage);
                    }
                    use objc2::MainThreadMarker;
                    use objc2::msg_send;
                    use objc2_app_kit::{NSApplication, NSBitmapImageRep};
                    use objc2_foundation::{NSData, NSDictionary, NSPoint, NSRect, NSSize};
                    if let Some(mtm) = MainThreadMarker::new() {
                        for window in NSApplication::sharedApplication(mtm).windows().iter() {
                            let wid: isize = unsafe { msg_send![&**window, windowNumber] };
                            if wid > 0 && window.title().to_string().contains("Keel") {
                                let rect = NSRect {
                                    origin: NSPoint {
                                        x: f64::INFINITY,
                                        y: f64::INFINITY,
                                    },
                                    size: NSSize {
                                        width: 0.0,
                                        height: 0.0,
                                    },
                                };
                                let cg_img = unsafe {
                                    CGWindowListCreateImage(
                                        rect, 8, /* kCGWindowListOptionIncludingWindow */
                                        wid as u32, 1, /* kCGWindowImageBoundsIgnoreFraming */
                                    )
                                };
                                if !cg_img.is_null() {
                                    let rep: *mut NSBitmapImageRep = unsafe {
                                        let alloc: *mut NSBitmapImageRep =
                                            msg_send![objc2::class!(NSBitmapImageRep), alloc];
                                        msg_send![alloc, initWithCGImage: cg_img]
                                    };
                                    unsafe { CGImageRelease(cg_img) };
                                    if !rep.is_null() {
                                        let png_type: usize = 4; // NSBitmapImageFileTypePNG = 4
                                        let props = NSDictionary::<
                                            objc2_foundation::NSString,
                                            objc2::runtime::AnyObject,
                                        >::new(
                                        );
                                        let data: *mut NSData = unsafe {
                                            msg_send![rep, representationUsingType: png_type, properties: &*props]
                                        };
                                        if !data.is_null() {
                                            let bytes: *const std::ffi::c_void =
                                                unsafe { msg_send![data, bytes] };
                                            let len: usize = unsafe { msg_send![data, length] };
                                            let slice = unsafe {
                                                std::slice::from_raw_parts(bytes as *const u8, len)
                                            };
                                            if std::fs::write(path, slice).is_ok() {
                                                return format!(
                                                    "saved bitmap to {path} ({} bytes)",
                                                    len
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                "error: no macos window".into()
            }
            other => return format!("error: unknown command {other:?}"),
        };
        cx.notify();
        // gpui draws no frames while the window is hidden: draw them here
        // until the change's animations settle, so a screenshot shows it.
        cx.spawn(async move |_, cx| {
            for _ in 0..45 {
                crate::dev_control::redraw();
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
            }
        })
        .detach();
        files_out.unwrap_or_else(|| answer.to_string())
    }

    fn dev_state(&self, cx: &Context<Self>) -> String {
        let chat = self
            .current_chat
            .as_ref()
            .and_then(|id| self.state.chats.get(id));
        let picker = self.open_picker().map(|p| match p {
            Picker::Model => "model",
            Picker::Branch => "branch",
            Picker::Folder => "folder",
            Picker::Repo => "repo",
        });
        let models: Vec<_> = self
            .models
            .get(&self.selected_harness)
            .map(|l| {
                l.models
                    .iter()
                    .map(|m| json!({"id": m.id, "name": m.name}))
                    .collect()
            })
            .unwrap_or_default();
        json!({
            "chat": self.current_chat,
            "title": chat.map(|c| c.title.clone()),
            "status": chat.map(|c| format!("{:?}", c.status)),
            "prompt_suggestion": self.prompt_suggestion,
            "harness": self.selected_harness,
            "model": self.selected_model,
            "model_label": self.model_label(),
            "effort": self.selected_effort,
            "composer": self.composer.read(cx).value().to_string(),
            "picker": picker,
            "project": self.projects.state_json(),
            "settings_open": self.settings_open,
            "skills_open": self.skills.open,
            "skill_rows": self.skills.rows.as_ref().and_then(|r| r.as_ref().ok()).map(|r| r.len()),
            "skill_open": self.skills.expanded_skill.iter().cloned().collect::<Vec<_>>(),
            "window_active": self.window_active,
            "models": models,
        })
        .to_string()
    }
}
