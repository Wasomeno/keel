//! The Keel window.

pub mod approval;
pub mod background;
pub mod browser;
pub mod browser_trace;
pub mod callout;
pub mod chart;
pub mod choices;
pub mod code_block;
pub mod compare;
pub mod composer;
pub mod dev_control;
pub mod diagram;
pub mod dotify;
pub mod file_changes;
pub mod files;
pub mod flow;
pub mod frame_probe;
pub mod icons;
pub mod keys;
pub mod markdown;
pub mod mention;
pub mod mermaid;
pub mod mermaid_viewer;
pub mod model_menu;
pub mod motion;
pub mod mr;
pub mod mr_card;
pub mod notify;
pub mod platform_notify;
pub mod runnable;
pub mod state;
pub mod stats;
pub mod steps;
pub mod style;
pub mod syntax;
pub mod terminal;
pub mod test_run;
pub mod thumbs;
pub mod timer;
pub mod trace;
pub mod trace_view;
pub mod transcript;
pub mod turn;
pub mod usage;
pub mod view;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, point, px, size,
};
use gpui_platform::application;
use keel_rpc::{RpcClient, ServerGuard};
use std::path::PathBuf;

pub use state::AppState;
pub use view::KeelWindow;

/// Launch the Keel window.
/// `on_quit` runs once before the process exits (the engine flushes there).
pub fn run(
    data_dir: PathBuf,
    guard: Option<ServerGuard>,
    port: u16,
    on_quit: impl FnOnce() + 'static,
) {
    let mut guard = guard;
    let mut on_quit = Some(on_quit);
    let app = application().with_assets(icons::Assets);
    // Debug builds: `ui-dev.sock` drives the window from a script (see `dev_control`).
    #[cfg(debug_assertions)]
    dev_control::start(&data_dir);
    // `keel://open-chat?id=…` and `keel://chat/…` (registered by the bundle).
    app.on_open_urls(|urls| {
        for url in urls {
            if let Some(id) = notify::chat_from_url(&url) {
                platform_notify::request_open(id);
            }
        }
    });
    app.run(move |cx: &mut App| {
        crate::browser::cef::ensure_cef_initialized();
        // The new-chat headline's face (see `view::main_pane`).
        let _ = cx
            .text_system()
            .add_fonts(vec![std::borrow::Cow::Borrowed(include_bytes!(
                "../assets/fonts/hero-title.ttf"
            ))]);
        style::load_mono(cx);
        if let Some(on_quit) = on_quit.take() {
            let mut on_quit = Some(on_quit);
            cx.on_app_quit(move |_| {
                if let Some(on_quit) = on_quit.take() {
                    on_quit();
                }
                async {}
            })
            .detach();
        }
        // Closing the only window quits, so the quit hook runs.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Some(ref g) = guard {
            gpui_tokio::init_from_handle(cx, g.handle());
        } else {
            gpui_tokio::init(cx);
        }

        let client = if let Some(ref g) = guard {
            if let Some(duplex) = g.take_duplex() {
                g.block_on(RpcClient::from_duplex(duplex)).ok()
            } else {
                None
            }
        } else {
            let token_path = data_dir.join("ipc.token");
            let token = std::fs::read_to_string(token_path)
                .unwrap_or_default()
                .trim()
                .to_string();
            let handle = gpui_tokio::Tokio::handle(cx);
            handle.block_on(RpcClient::connect_ws(port, &token)).ok()
        };

        let device_id = client.as_ref().map(|c| c.device_id()).unwrap_or_default();
        let app_state = AppState::new(data_dir.clone(), device_id);

        gpui_base::init(cx);
        keys::init(cx);

        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // Content runs under a transparent title bar; the traffic
                // lights sit inside the sidebar header.
                titlebar: Some(TitlebarOptions {
                    title: Some("Keel".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(14.), px(14.))),
                }),
                window_background: WindowBackgroundAppearance::Opaque,
                window_min_size: Some(size(px(640.), px(420.))),
                ..WindowOptions::default()
            },
            |window, cx| {
                window.set_window_title("Keel");
                cx.new(|cx| KeelWindow::new(app_state, client, window, cx))
            },
        )
        .expect("failed to open the Keel window");
        #[cfg(target_os = "macos")]
        triple_buffer();
        // Debug verification: KEEL_NO_ACTIVATE=1 skips bringing Keel forward so
        // scripted relaunches do not steal focus from whatever the user is doing.
        #[cfg(debug_assertions)]
        let activate = std::env::var_os("KEEL_NO_ACTIVATE").is_none();
        #[cfg(not(debug_assertions))]
        let activate = true;
        if activate {
            cx.activate(true);
        }

        if let Some(g) = guard.take() {
            cx.set_global(GuardHolder(g));
        }
    });
}

/// gpui keeps its Metal layer to 2 drawables. Keel draws as soon as an
/// input event arrives, so a frame finishing after the compositor's cutoff
/// leaves `nextDrawable` blocked for a whole refresh, and scrolling settled
/// into lock-step 30 fps on a 60 Hz screen. A third drawable (CAMetalLayer's
/// default) absorbs a late frame instead.
#[cfg(target_os = "macos")]
fn triple_buffer() {
    use objc2::{
        msg_send,
        runtime::{AnyClass, AnyObject},
    };
    use objc2_app_kit::{NSApplication, NSView};
    fn visit(view: &NSView, metal: &AnyClass) {
        let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
        if let Some(layer) = unsafe { layer.as_ref() } {
            let is_metal: bool = unsafe { msg_send![layer, isKindOfClass: metal] };
            if is_metal {
                let _: () = unsafe { msg_send![layer, setMaximumDrawableCount: 3usize] };
            }
        }
        for sub in view.subviews().iter() {
            visit(&sub, metal);
        }
    }
    let (Some(mtm), Some(metal)) = (
        objc2::MainThreadMarker::new(),
        AnyClass::get(c"CAMetalLayer"),
    ) else {
        return;
    };
    for window in NSApplication::sharedApplication(mtm).windows().iter() {
        if let Some(view) = window.contentView() {
            visit(&view, metal);
        }
    }
}

#[allow(dead_code)]
struct GuardHolder(ServerGuard);
impl gpui::Global for GuardHolder {}
