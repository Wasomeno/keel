//! The CEF-backed browser page: off-screen rendering (OSR) into a pixel buffer
//! drawn inside gpui. Real Chromium in its own process, agent tools driven over CDP.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use cef::*;
use gpui::{Bounds, Pixels, Point};

use super::agent::{ConsoleEntry, DialogInfo, DialogKind, site_allowed};
use super::{PageState, allowed_navigation, forwarded_key};

pub type Sender = tokio::sync::mpsc::UnboundedSender<NativeEvent>;

/// Size the page lays out at while it runs with the pane closed: a desktop
/// window, so pages and screenshots look like a normal browser's.
pub const BACKGROUND_SIZE: (f64, f64) = (1280., 800.);

/// How long a resize waits for a paint at the new size before the settle
/// line gives up. The line clears earlier, on that paint.
const LAYOUT_PAINT_WAIT: Duration = Duration::from_millis(800);

/// `due` is the device-pixel size a resize is waiting on. A paint of that
/// size catches up; a paint of the previous size does not.
pub(crate) fn settle_paint(due: Option<(u32, u32)>, painted: (u32, u32)) -> Option<(u32, u32)> {
    match due {
        Some(size) if size == painted => None,
        other => other,
    }
}

/// Keel's agent library ([`js/agent.js`]): snapshot with refs, actions,
/// wait checks. Evaluated in the page's JS context.
const AGENT_JS: &str = include_str!("js/agent.js");

/// A full-page frame: scroll to `y`. Called as `(FRAME_JS)(y)`.
const FRAME_JS: &str = r#"function(y){
  window.scrollTo({left: 0, top: y, behavior: 'instant'});
  return JSON.stringify({y: scrollY});
}"#;

/// Before frames after the first: hide fixed and sticky elements, so
/// headers don't repeat down the stitch.
const HIDE_FIXED_JS: &str = r#"(function(){
  if (!document.getElementById('__keel_hide_style')) {
    var st = document.createElement('style');
    st.id = '__keel_hide_style';
    st.textContent = '[data-keel-hide], [data-keel-hide] * { visibility: hidden !important; opacity: 0 !important; }';
    (document.head || document.documentElement).appendChild(st);
  }
  var all = document.querySelectorAll('*'), added = 0;
  for (var i = 0; i < all.length && i < 80000; i++) {
    var el = all[i];
    if (el === document.documentElement || el === document.body || el.hasAttribute('data-keel-hide')) continue;
    var pos = getComputedStyle(el).position;
    if (pos === 'fixed' || pos === 'sticky') { el.setAttribute('data-keel-hide', ''); added++; }
  }
  return JSON.stringify({total: all.length, added: added});
})()"#;

/// After a full-page screenshot: show what was hidden, scroll back.
const RESTORE_JS: &str = r#"function(x, y){
  document.querySelectorAll('[data-keel-hide]').forEach(function(el){ el.removeAttribute('data-keel-hide'); });
  var st = document.getElementById('__keel_hide_style');
  if (st) st.remove();
  window.scrollTo({left: x, top: y, behavior: 'instant'});
}"#;

/// Full-page prep: report the document size and initial scroll position.
const PAGE_SIZE_JS: &str = r#"(function(){
  var root = document.scrollingElement || document.documentElement;
  var b = document.body || root;
  var sx = scrollX, sy = scrollY;
  window.scrollTo({left: 0, top: 0, behavior: 'instant'});
  return JSON.stringify({
    w: innerWidth,
    h: Math.max(root.scrollHeight, b.scrollHeight, innerHeight),
    vh: innerHeight, sx: sx, sy: sy
  });
})()"#;

pub enum NativeEvent {
    /// Something the toolbar shows changed; read [`NativePage::state`].
    Changed,
    /// A link asked for a new window; it opens here instead.
    Open(String),
    /// A Keel shortcut typed while the page had focus.
    Key(gpui::Keystroke),
    /// An agent script ([`NativePage::eval_agent`]) finished: its sequence
    /// number and JSON outcome.
    Eval(u64, Result<String, String>),
    /// A host screenshot: PNG bytes and the page's width in points.
    Screenshot(Result<(Vec<u8>, u32), String>),
    /// Before a full-page screenshot: the page's size (JSON from
    /// [`PAGE_SIZE_JS`]); it has been scrolled to the top.
    PageSize(Result<String, String>),
    /// A full-page frame's scroll landed: JSON `{y}` (the real position,
    /// clamped at the bottom).
    Scrolled(Result<String, String>),
    /// An agent page opened a JavaScript dialog. `held`: a confirm/prompt
    /// waiting for [`NativePage::answer_dialog`] (the page is paused); else
    /// an alert, already accepted.
    Dialog { info: DialogInfo, held: bool },
    /// An agent page asked for a file chooser: `Some(n)` when it got the
    /// `n` files [`NativePage::set_upload`] staged, `None` when it was
    /// refused.
    FilePicker(Option<usize>),
    /// A console message or page error (untrusted page text).
    Console(ConsoleEntry),
    /// An agent page tried to leave the allowed sites; the load was stopped.
    Blocked(String),
    /// CEF has a new frame ready to be composited/painted into gpui.
    FrameReady,
    /// CEF background work woke the main thread to pump messages.
    Wake,
}

#[cfg(not(target_os = "macos"))]
static ACTIVE_WAKER_TX: Mutex<Vec<Sender>> = Mutex::new(Vec::new());
static CEF_INITIALIZED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
static LIBRARY_LOADER: std::sync::OnceLock<library_loader::LibraryLoader> =
    std::sync::OnceLock::new();

/// Set up CEF once at app start.
pub fn ensure_cef_initialized() {
    if CEF_INITIALIZED.load(Ordering::SeqCst) {
        return;
    }

    #[cfg(target_os = "macos")]
    LIBRARY_LOADER.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("keel"));
        // Try standard bundle location first
        let loader = library_loader::LibraryLoader::new(&exe, false);
        if loader.load() {
            loader
        } else {
            // Fall back to .cef vendor directory if running directly (e.g. in tests)
            let vendor = exe.parent().unwrap_or_else(|| Path::new(".")).join("../../.cef/cef_macos_aarch64/Chromium Embedded Framework.framework/Chromium Embedded Framework");
            if let Ok(canon) = vendor.canonicalize() {
                let direct = library_loader::LibraryLoader::new(&canon, false);
                direct.load();
                direct
            } else {
                loader
            }
        }
    });

    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let bph = KeelBrowserProcessHandler::new();
    let mut app = KeelApp::new(bph);

    let cache_dir = dirs_profile();
    let cache_path = cache_dir.to_string_lossy().into_owned();

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("keel"));
    let helper_path = resolve_helper_path(&exe);
    let framework_path = resolve_framework_path(&exe);
    let bundle_path = resolve_bundle_path(&exe);

    let mut settings = Settings {
        size: std::mem::size_of::<sys::_cef_settings_t>(),
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        multi_threaded_message_loop: 0,
        background_color: 0xFFFFFFFF,
        cache_path: CefString::from(cache_path.as_str()),
        root_cache_path: CefString::from(cache_path.as_str()),
        persist_session_cookies: 1,
        browser_subprocess_path: helper_path
            .as_deref()
            .map(CefString::from)
            .unwrap_or_default(),
        framework_dir_path: framework_path
            .as_deref()
            .map(CefString::from)
            .unwrap_or_default(),
        main_bundle_path: bundle_path
            .as_deref()
            .map(CefString::from)
            .unwrap_or_default(),
        ..unsafe { std::mem::zeroed() }
    };

    let args = args::Args::new();
    let code = initialize(
        Some(&args.as_main_args()),
        Some(&mut settings),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if code == 1 {
        CEF_INITIALIZED.store(true, Ordering::SeqCst);
        tracing::info!("CEF initialized successfully");
    } else {
        tracing::warn!("CEF initialization returned {code}");
    }
}

fn resolve_helper_path(exe: &Path) -> Option<String> {
    // If inside an app bundle:
    // target/KeelDebug.app/Contents/MacOS/keel -> target/KeelDebug.app/Contents/Frameworks/Keel Debug Helper.app/Contents/MacOS/keel-helper
    let in_bundle = exe
        .parent()
        .and_then(|m| m.parent())
        .map(|c| c.join("Frameworks/Keel Debug Helper.app/Contents/MacOS/keel-helper"));
    if let Some(p) = in_bundle.filter(|p| p.is_file()) {
        return Some(p.to_string_lossy().into_owned());
    }
    // Beside executable:
    let beside = exe.parent().map(|d| d.join("keel-helper"));
    if let Some(p) = beside.filter(|p| p.is_file()) {
        return Some(p.to_string_lossy().into_owned());
    }
    None
}

fn resolve_framework_path(exe: &Path) -> Option<String> {
    let in_bundle = exe
        .parent()
        .and_then(|m| m.parent())
        .map(|c| c.join("Frameworks/Chromium Embedded Framework.framework"));
    if let Some(p) = in_bundle.filter(|p| p.exists()) {
        return Some(p.to_string_lossy().into_owned());
    }
    let local = PathBuf::from(".cef/cef_macos_aarch64/Chromium Embedded Framework.framework");
    if local.exists() {
        return Some(
            local
                .canonicalize()
                .unwrap_or(local)
                .to_string_lossy()
                .into_owned(),
        );
    }
    None
}

fn resolve_bundle_path(exe: &Path) -> Option<String> {
    let app = exe
        .parent()
        .and_then(|m| m.parent())
        .and_then(|c| c.parent());
    if let Some(p) = app.filter(|p| p.extension().is_some_and(|e| e == "app")) {
        return Some(p.to_string_lossy().into_owned());
    }
    None
}

fn dirs_profile() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library/Application Support/Keel/browser"));
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(|h| PathBuf::from(h).join("Keel/browser"))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/Keel/browser"))
        });
    let dir = base.unwrap_or_else(|| PathBuf::from("/tmp/keel-browser"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

wrap_app! {
    struct KeelApp {
        bph: BrowserProcessHandler,
    }
    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(cl) = command_line {
                cl.append_switch(Some(&CefString::from("use-mock-keychain")));
                cl.append_switch_with_value(
                    Some(&CefString::from("password-store")),
                    Some(&CefString::from("basic")),
                );
            }
        }
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(self.bph.clone())
        }
    }
}

#[cfg(target_os = "macos")]
mod mac_dispatch {
    #[link(name = "System", kind = "dylib")]
    unsafe extern "C" {
        static _dispatch_main_q: std::ffi::c_void;
        fn dispatch_after_f(
            when: u64,
            queue: *const std::ffi::c_void,
            context: *mut std::ffi::c_void,
            work: extern "C" fn(*mut std::ffi::c_void),
        );
        fn dispatch_time(when: u64, delta: i64) -> u64;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetMain() -> *mut std::ffi::c_void;
        fn CFRunLoopWakeUp(rl: *mut std::ffi::c_void);
    }

    /// `DoMessageLoopWork` is not re-entrant. Chromium drains the main queue
    /// from inside it, so a `dispatch_async` scheduled there runs the pump
    /// nested, that call no-ops, and CEF never asks for another one. Every
    /// navigation then sits on `loading` with a blank page.
    static PUMPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static PUMP_AGAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    extern "C" fn cef_pump_work(_context: *mut std::ffi::c_void) {
        pump();
    }

    pub fn pump() {
        use std::sync::atomic::Ordering;
        if PUMPING.swap(true, Ordering::SeqCst) {
            PUMP_AGAIN.store(true, Ordering::SeqCst);
            return;
        }
        cef::do_message_loop_work();
        PUMPING.store(false, Ordering::SeqCst);
        if PUMP_AGAIN.swap(false, Ordering::SeqCst) {
            schedule_work(0);
        }
    }

    pub fn schedule_work(delay_ms: i64) {
        unsafe {
            let queue = std::ptr::addr_of!(_dispatch_main_q);
            // Never `dispatch_async` (delay 0): that is the re-entrant path
            // above. One millisecond still counts as "reasonably soon".
            let delay_ms = if delay_ms <= 0 { 1 } else { delay_ms };
            let when = dispatch_time(0, delay_ms.saturating_mul(1_000_000));
            dispatch_after_f(when, queue, std::ptr::null_mut(), cef_pump_work);
            CFRunLoopWakeUp(CFRunLoopGetMain());
        }
    }

    /// Run `work` on the main queue after the current UI turn. A timer, not
    /// `dispatch_async`, so Chromium does not drain it inside `DoMessageLoopWork`.
    pub fn after(context: *mut std::ffi::c_void, work: extern "C" fn(*mut std::ffi::c_void)) {
        unsafe {
            let queue = std::ptr::addr_of!(_dispatch_main_q);
            let when = dispatch_time(0, 1_000_000);
            dispatch_after_f(when, queue, context, work);
            CFRunLoopWakeUp(CFRunLoopGetMain());
        }
    }
}

wrap_browser_process_handler! {
    struct KeelBrowserProcessHandler;
    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            #[cfg(target_os = "macos")]
            mac_dispatch::schedule_work(delay_ms);

            // On macOS `mac_dispatch::schedule_work` already schedules the main
            // queue directly; only wake UI listeners on platforms without it.
            #[cfg(not(target_os = "macos"))]
            if delay_ms <= 0 {
                let mut wakers = ACTIVE_WAKER_TX.lock().unwrap_or_else(|p| p.into_inner());
                wakers.retain(|tx| tx.send(NativeEvent::Wake).is_ok());
            }
        }
    }
}

fn device_px(dip_w: u32, dip_h: u32, scale: f32) -> (u32, u32) {
    let px = |dip: u32| ((dip as f32) * scale).round().max(1.) as u32;
    (px(dip_w), px(dip_h))
}

struct FrameState {
    /// Requested view, in CSS pixels. `view_rect` returns this and paint
    /// never writes it: feeding the buffer size back here rounds off by a
    /// pixel and the page shakes on every resize.
    dip_w: u32,
    dip_h: u32,
    /// Last paint buffer, in device pixels.
    buf_w: u32,
    buf_h: u32,
    scale: f32,
    buffer: Vec<u8>,
    has_new: bool,
    generation: u64,
}

struct SharedState {
    url: Option<String>,
    title: String,
    loading: bool,
    can_back: bool,
    can_forward: bool,
    progress: f32,
    cursor: gpui::CursorStyle,
    sites: Option<Vec<String>>,
    upload: Option<Vec<PathBuf>>,
    agent_mode: bool,
}

wrap_render_handler! {
    struct KeelRenderHandler {
        frame_state: Arc<Mutex<FrameState>>,
        tx: Sender,
    }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let fs = self.frame_state.lock().unwrap();
                rect.x = 0;
                rect.y = 0;
                rect.width = fs.dip_w.max(1) as i32;
                rect.height = fs.dip_h.max(1) as i32;
            }
        }

        fn screen_info(&self, _browser: Option<&mut Browser>, screen_info: Option<&mut ScreenInfo>) -> ::std::os::raw::c_int {
            if let Some(info) = screen_info {
                let fs = self.frame_state.lock().unwrap();
                info.device_scale_factor = fs.scale;
                return 1;
            }
            0
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if type_ != PaintElementType::VIEW || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let size = (width * height * 4) as usize;
            let slice = unsafe { std::slice::from_raw_parts(buffer, size) };
            let mut fs = self.frame_state.lock().unwrap();
            fs.buf_w = width as u32;
            fs.buf_h = height as u32;
            fs.buffer.resize(size, 0);
            fs.buffer.copy_from_slice(slice);
            fs.has_new = true;
            fs.generation += 1;
            let _ = self.tx.send(NativeEvent::FrameReady);
        }
    }
}

wrap_load_handler! {
    struct KeelLoadHandler {
        shared: Arc<Mutex<SharedState>>,
        tx: Sender,
    }
    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            {
                let mut state = self.shared.lock().unwrap();
                state.loading = is_loading != 0;
                state.can_back = can_go_back != 0;
                state.can_forward = can_go_forward != 0;
                if is_loading == 0 {
                    state.progress = 1.0;
                } else if state.progress >= 1.0 {
                    state.progress = 0.1;
                }
            }
            let _ = self.tx.send(NativeEvent::Changed);
        }

        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: ::std::os::raw::c_int,
        ) {
            if frame.map_or(false, |f| f.is_main() != 0) {
                let mut state = self.shared.lock().unwrap();
                state.loading = false;
                state.progress = 1.0;
                let _ = self.tx.send(NativeEvent::Changed);
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            let main = frame.as_ref().is_some_and(|f| f.is_main() != 0);
            tracing::warn!(
                target: "keel_ui::browser::cef",
                ?error_code,
                main,
                url = failed_url.map(|s| s.to_string()),
                text = error_text.map(|s| s.to_string()),
                "load error"
            );
            // A new navigation aborts the previous one; that is not a failure.
            // A real error must clear `loading`, or the bar stays up forever
            // when CEF does not follow with `on_loading_state_change(0)`.
            if main && error_code != Errorcode::ABORTED {
                let mut state = self.shared.lock().unwrap();
                state.loading = false;
                state.progress = 1.0;
                let _ = self.tx.send(NativeEvent::Changed);
            }
        }
    }
}

wrap_display_handler! {
    struct KeelDisplayHandler {
        shared: Arc<Mutex<SharedState>>,
        tx: Sender,
        started: std::time::Instant,
    }
    impl DisplayHandler {
        fn on_address_change(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            if frame.map_or(false, |f| f.is_main() != 0) {
                if let Some(u) = url {
                    let s = u.to_string();
                    self.shared.lock().unwrap().url = Some(s);
                    let _ = self.tx.send(NativeEvent::Changed);
                }
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if let Some(t) = title {
                self.shared.lock().unwrap().title = t.to_string();
                let _ = self.tx.send(NativeEvent::Changed);
            }
        }

        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            level: LogSeverity,
            message: Option<&CefString>,
            source: Option<&CefString>,
            line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let msg = message.map(|m| m.to_string()).unwrap_or_default();
            let src = source.map(|s| s.to_string()).unwrap_or_default();
            let entry = ConsoleEntry {
                level: match level {
                    LogSeverity::ERROR | LogSeverity::FATAL => "error".into(),
                    LogSeverity::WARNING => "warning".into(),
                    _ => "info".into(),
                },
                text: if src.is_empty() { msg } else { format!("{msg} ({src}:{line})") },
                at: self.started.elapsed().as_secs_f32(),
                frame: String::new(),
            };
            let _ = self.tx.send(NativeEvent::Console(entry));
            0
        }

        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: *mut u8,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> ::std::os::raw::c_int {
            let next = cef_cursor_to_gpui(type_);
            let changed = {
                let mut s = self.shared.lock().unwrap();
                if s.cursor != next {
                    s.cursor = next;
                    true
                } else {
                    false
                }
            };
            if changed {
                let _ = self.tx.send(NativeEvent::Changed);
            }
            1
        }
    }
}

wrap_life_span_handler! {
    struct KeelLifeSpanHandler {
        tx: Sender,
    }
    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Some(url) = target_url {
                let _ = self.tx.send(NativeEvent::Open(url.to_string()));
            }
            1
        }
    }
}

wrap_jsdialog_handler! {
    struct KeelJsdialogHandler {
        held: Arc<Mutex<Option<(DialogKind, JsdialogCallback)>>>,
        tx: Sender,
    }
    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            origin_url: Option<&CefString>,
            dialog_type: JsdialogType,
            message_text: Option<&CefString>,
            default_prompt_text: Option<&CefString>,
            callback: Option<&mut JsdialogCallback>,
            _suppress_message: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let kind = match dialog_type {
                JsdialogType::ALERT => DialogKind::Alert,
                JsdialogType::CONFIRM => DialogKind::Confirm,
                JsdialogType::PROMPT => DialogKind::Prompt,
                _ => DialogKind::Alert,
            };
            let info = DialogInfo {
                kind,
                message: message_text.map(|m| m.to_string()).unwrap_or_default(),
                default_text: default_prompt_text.map(|p| p.to_string()),
                site: origin_url.map(|o| o.to_string()).unwrap_or_default(),
            };
            match dialog_type {
                JsdialogType::ALERT => {
                    let _ = self.tx.send(NativeEvent::Dialog { info, held: false });
                    0
                }
                _ => {
                    if let Some(cb) = callback {
                        *self.held.lock().unwrap() = Some((kind, cb.clone()));
                    }
                    let _ = self.tx.send(NativeEvent::Dialog { info, held: true });
                    1
                }
            }
        }
    }
}

wrap_request_handler! {
    struct KeelRequestHandler {
        shared: Arc<Mutex<SharedState>>,
        tx: Sender,
    }
    impl RequestHandler {
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: ::std::os::raw::c_int,
            _is_redirect: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let Some(req) = request else { return 0 };
            let raw_url = req.url();
            let url = CefString::from(&raw_url).to_string();
            tracing::debug!(target: "keel_ui::browser::cef", url = ?url, "on_before_browse");
            if !url.is_empty() && !allowed_navigation(&url) {
                tracing::debug!(target: "keel_ui::browser::cef", url = ?url, "blocked navigation");
                return 1;
            }
            let state = self.shared.lock().unwrap();
            if state.agent_mode {
                if let Some(sites) = &state.sites {
                    if !site_allowed(&url, sites) {
                        let _ = self.tx.send(NativeEvent::Blocked(url));
                        return 1;
                    }
                }
            }
            0
        }
    }
}

wrap_context_menu_handler! {
    struct KeelContextMenuHandler;
    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            if let Some(m) = model {
                m.clear();
            }
        }
    }
}

#[cfg(target_os = "macos")]
wrap_keyboard_handler! {
    struct KeelKeyboardHandler;
    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            _browser: Option<&mut Browser>,
            _event: Option<&KeyEvent>,
            _os_event: *mut u8,
            is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Some(shortcut) = is_keyboard_shortcut {
                *shortcut = 0;
            }
            0
        }

        fn on_key_event(
            &self,
            _browser: Option<&mut Browser>,
            _event: Option<&KeyEvent>,
            _os_event: *mut u8,
        ) -> ::std::os::raw::c_int {
            // GPUI already matched the menu bar and keymap before forwarding
            // to CEF; returning 1 stops Chromium from re-entering `[NSApp mainMenu]`
            // while GPUI may hold an active borrow.
            1
        }
    }
}

type CdpCallback = Box<dyn FnOnce(Result<serde_json::Value, String>) + Send + 'static>;

wrap_dev_tools_message_observer! {
    struct KeelDevToolsObserver {
        callbacks: Arc<Mutex<HashMap<u64, CdpCallback>>>,
    }
    impl DevToolsMessageObserver {
        fn on_dev_tools_message(
            &self,
            _browser: Option<&mut Browser>,
            message: Option<&[u8]>,
        ) -> ::std::os::raw::c_int {
            let Some(slice) = message else {
                return 0;
            };
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(slice) {
                if let Some(id) = val.get("id").and_then(|i| i.as_u64()) {
                    let cb = self.callbacks.lock().unwrap().remove(&id);
                    if let Some(cb) = cb {
                        if let Some(err) = val.get("error") {
                            cb(Err(err["message"].as_str().unwrap_or("CDP error").to_string()));
                        } else {
                            cb(Ok(val.get("result").cloned().unwrap_or(serde_json::Value::Null)));
                        }
                        return 1;
                    }
                }
            }
            0
        }

        fn on_dev_tools_method_result(
            &self,
            _browser: Option<&mut Browser>,
            message_id: ::std::os::raw::c_int,
            success: ::std::os::raw::c_int,
            result: Option<&[u8]>,
        ) {
            let id = message_id as u64;
            let cb = self.callbacks.lock().unwrap().remove(&id);
            if let Some(cb) = cb {
                if success != 0 {
                    let val = result.and_then(|r| serde_json::from_slice::<serde_json::Value>(r).ok()).unwrap_or(serde_json::Value::Null);
                    cb(Ok(val));
                } else {
                    let err = result.and_then(|r| serde_json::from_slice::<serde_json::Value>(r).ok())
                        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
                        .unwrap_or_else(|| "CDP error".to_string());
                    cb(Err(err));
                }
            }
        }
    }
}

wrap_client! {
    struct KeelClient {
        render: RenderHandler,
        load: LoadHandler,
        display: DisplayHandler,
        life_span: LifeSpanHandler,
        jsdialog: JsdialogHandler,
        request: RequestHandler,
        context_menu: ContextMenuHandler,
    }
    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }
        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }
        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.jsdialog.clone())
        }
        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }
        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(self.context_menu.clone())
        }
        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            #[cfg(target_os = "macos")]
            {
                Some(KeelKeyboardHandler::new())
            }
            #[cfg(not(target_os = "macos"))]
            {
                None
            }
        }
    }
}

pub struct NativePage {
    browser: Browser,
    frame_state: Arc<Mutex<FrameState>>,
    shared_state: Arc<Mutex<SharedState>>,
    current_frame: RefCell<Option<Arc<gpui::RenderImage>>>,
    dropped_frames: RefCell<Vec<Arc<gpui::RenderImage>>>,
    origin: Cell<Point<Pixels>>,
    tx: Sender,
    held_dialog: Arc<Mutex<Option<(DialogKind, JsdialogCallback)>>>,
    cdp_callbacks: Arc<Mutex<HashMap<u64, CdpCallback>>>,
    _devtools_reg: Option<Registration>,
    _devtools_obs: Option<DevToolsMessageObserver>,
    next_cdp_id: AtomicU64,
    scale: Cell<f32>,
    /// Device-pixel size last given to CEF. Compared in `sync`.
    size: Cell<(u32, u32)>,
    /// Drag in progress: keep the last frame and scale it, instead of
    /// reflowing Chromium on every pointer move.
    resizing: Cell<bool>,
    /// A gesture just ended. True until `sync` either resizes or finds the
    /// size unchanged, so the settle line does not drop for the frame
    /// before the resize lands.
    layout_armed: Cell<bool>,
    /// Device-pixel size of a resize whose matching paint has not arrived,
    /// and when the wait started.
    layout_due: Cell<Option<((u32, u32), Instant)>>,
    /// `set_windowless_frame_rate` last sent. Calling it every frame
    /// restarts the compositor clock and the picture jitters.
    frame_rate: Cell<i32>,
    background: Cell<bool>,
    shown: Cell<bool>,
    parked: Cell<bool>,
    focused: Cell<bool>,
    pressed_button: Cell<Option<gpui::MouseButton>>,
}

impl NativePage {
    pub fn new(_window: &gpui::Window, tx: Sender, _radius: f32) -> Result<Self, String> {
        ensure_cef_initialized();

        let (init_w, init_h) = (BACKGROUND_SIZE.0 as u32, BACKGROUND_SIZE.1 as u32);
        let frame_state = Arc::new(Mutex::new(FrameState {
            dip_w: init_w,
            dip_h: init_h,
            buf_w: 0,
            buf_h: 0,
            scale: 2.0,
            buffer: Vec::new(),
            has_new: false,
            generation: 0,
        }));
        let shared_state = Arc::new(Mutex::new(SharedState {
            url: None,
            title: String::new(),
            loading: false,
            can_back: false,
            can_forward: false,
            progress: 0.0,
            cursor: gpui::CursorStyle::Arrow,
            sites: None,
            upload: None,
            agent_mode: false,
        }));
        let held_dialog = Arc::new(Mutex::new(None));
        let cdp_callbacks: Arc<Mutex<HashMap<u64, CdpCallback>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let render = KeelRenderHandler::new(frame_state.clone(), tx.clone());
        let load = KeelLoadHandler::new(shared_state.clone(), tx.clone());
        let display =
            KeelDisplayHandler::new(shared_state.clone(), tx.clone(), std::time::Instant::now());
        let life_span = KeelLifeSpanHandler::new(tx.clone());
        let jsdialog = KeelJsdialogHandler::new(held_dialog.clone(), tx.clone());
        let request = KeelRequestHandler::new(shared_state.clone(), tx.clone());
        let context_menu = KeelContextMenuHandler::new();

        let mut client = KeelClient::new(
            render,
            load,
            display,
            life_span,
            jsdialog,
            request,
            context_menu,
        );

        // Alloy + a null parent. Windowless rendering ignores Chrome style;
        // `set_as_windowless` is what actually marks the browser off-screen.
        let window_info = WindowInfo::default().set_as_windowless(std::ptr::null_mut());
        let browser_settings = BrowserSettings {
            size: std::mem::size_of::<sys::_cef_browser_settings_t>(),
            windowless_frame_rate: 60,
            background_color: 0xFFFFFFFF,
            javascript: sys::cef_state_t::STATE_ENABLED.into(),
            webgl: sys::cef_state_t::STATE_ENABLED.into(),
            ..unsafe { std::mem::zeroed() }
        };

        let browser = browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from("about:blank")),
            Some(&browser_settings),
            None,
            None,
        )
        .ok_or_else(|| "failed to create CEF browser".to_string())?;

        let (devtools_reg, devtools_obs) = if let Some(host) = browser.host() {
            let mut observer = KeelDevToolsObserver::new(cdp_callbacks.clone());
            let reg = host.add_dev_tools_message_observer(Some(&mut observer));
            host.was_hidden(0);
            host.set_windowless_frame_rate(60);
            (reg, Some(observer))
        } else {
            (None, None)
        };

        #[cfg(not(target_os = "macos"))]
        ACTIVE_WAKER_TX
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(tx.clone());

        Ok(Self {
            browser,
            frame_state,
            shared_state,
            current_frame: RefCell::new(None),
            dropped_frames: RefCell::new(Vec::new()),
            origin: Cell::new(Point::default()),
            tx,
            held_dialog,
            cdp_callbacks,
            _devtools_reg: devtools_reg,
            _devtools_obs: devtools_obs,
            next_cdp_id: AtomicU64::new(1),
            scale: Cell::new(2.0),
            size: Cell::new((
                (init_w as f32 * 2.).round() as u32,
                (init_h as f32 * 2.).round() as u32,
            )),
            resizing: Cell::new(false),
            layout_armed: Cell::new(false),
            layout_due: Cell::new(None),
            frame_rate: Cell::new(60),
            background: Cell::new(false),
            shown: Cell::new(false),
            parked: Cell::new(false),
            focused: Cell::new(false),
            pressed_button: Cell::new(None),
        })
    }

    pub fn cursor_style(&self) -> gpui::CursorStyle {
        self.shared_state.lock().unwrap().cursor
    }

    pub fn state(&self) -> PageState {
        let s = self.shared_state.lock().unwrap();
        PageState {
            url: s.url.clone(),
            title: s.title.clone(),
            loading: s.loading,
            can_back: s.can_back,
            can_forward: s.can_forward,
            progress: s.progress,
        }
    }

    pub fn load(&self, url: &str) {
        if !allowed_navigation(url) {
            return;
        }
        tracing::info!(target: "keel_ui::browser::cef", url, "load");
        {
            let mut s = self.shared_state.lock().unwrap();
            s.loading = true;
            s.url = Some(url.to_string());
            s.progress = 0.05;
        }
        if let Some(host) = self.browser.host() {
            host.was_hidden(0);
        }
        // `load_url` only. A second `Page.navigate` aborts it and then waits
        // on a DevTools agent that was never enabled, so the load never ends.
        if let Some(frame) = self.browser.main_frame() {
            frame.load_url(Some(&CefString::from(url)));
        } else {
            tracing::warn!(target: "keel_ui::browser::cef", url, "load: no main frame");
        }
        // Pump on the next turn. Doing it here is inside gpui's borrow, and
        // Chromium re-enters AppKit from that call.
        self.spin_soon();
    }

    pub fn load_html(&self, html: &str) {
        let data = format!("data:text/html;charset=utf-8,{}", urlencoding_simple(html));
        self.load(&data);
    }

    pub fn reload(&self) {
        self.browser.reload();
    }

    pub fn stop(&self) {
        self.browser.stop_load();
    }

    pub fn go_back(&self) {
        self.browser.go_back();
    }

    pub fn go_forward(&self) {
        self.browser.go_forward();
    }

    pub fn set_background(&self, on: bool) {
        self.background.set(on);
        if let Some(host) = self.browser.host() {
            if on {
                host.was_hidden(0);
            } else if !self.shown.get() {
                host.was_hidden(1);
            }
        }
        if on {
            self.set_frame_rate(30);
        } else if !self.shown.get() {
            self.set_frame_rate(1);
        }
    }

    pub fn background(&self) -> bool {
        self.background.get()
    }

    pub fn set_agent(&self, on: bool, sites: &[String]) {
        let mut s = self.shared_state.lock().unwrap();
        s.agent_mode = on;
        s.sites = if sites.is_empty() {
            None
        } else {
            Some(sites.to_vec())
        };
    }

    pub fn set_scale_factor(&self, scale: f32) {
        if (self.scale.get() - scale).abs() > 0.01 {
            self.scale.set(scale);
            let (dip_w, dip_h) = {
                let mut fs = self.frame_state.lock().unwrap();
                fs.scale = scale;
                (fs.dip_w, fs.dip_h)
            };
            self.size.set(device_px(dip_w, dip_h, scale));
            if self.current_frame.borrow().is_some() {
                self.layout_due.set(Some((self.size.get(), Instant::now())));
            }
            if let Some(host) = self.browser.host() {
                host.notify_screen_info_changed();
                host.was_resized();
            }
        }
    }

    /// The pane edge is being dragged. Chromium keeps its current layout;
    /// the last frame scales with the pane until the drag ends.
    pub fn set_resizing(&self, on: bool) {
        self.resizing.set(on);
        if on {
            // The bitmap is scaled on purpose for the gesture. A paint from
            // the size before the drag must not clear the next wait.
            self.layout_armed.set(false);
            self.layout_due.set(None);
        }
    }

    /// The gesture ended (or the width jumped). The next `sync` resizes.
    /// No-op until a frame exists: there is nothing stale to cover.
    pub fn arm_layout(&self) {
        if self.current_frame.borrow().is_some() {
            self.layout_armed.set(true);
        }
    }

    /// A loaded page is still showing a bitmap from the previous size.
    pub fn layout_pending(&self) -> bool {
        if self.layout_armed.get() {
            return true;
        }
        match self.layout_due.get() {
            Some((_, at)) if at.elapsed() < LAYOUT_PAINT_WAIT => true,
            Some(_) => {
                self.layout_due.set(None);
                false
            }
            None => false,
        }
    }

    pub fn sync(
        &self,
        bounds: Bounds<Pixels>,
        _mask: Bounds<Pixels>,
        _alpha: f32,
        _passthrough: bool,
    ) {
        self.origin.set(bounds.origin);
        let dip_w = f32::from(bounds.size.width).round().max(1.) as u32;
        let dip_h = f32::from(bounds.size.height).round().max(1.) as u32;
        let scale = self.scale.get();
        let target = device_px(dip_w, dip_h, scale);
        // While the pointer is down, a new Chromium layout every move is
        // what shakes. One resize lands when the drag ends.
        if !self.resizing.get() && self.size.get() != target {
            self.resize_dip(dip_w, dip_h, target);
        } else if self.layout_armed.get() {
            // The gesture ended on the size Chromium already has.
            self.layout_armed.set(false);
        }
        if !self.shown.get() {
            self.shown.set(true);
            if let Some(host) = self.browser.host() {
                host.was_hidden(0);
            }
        }
        self.set_frame_rate(60);
    }

    fn resize_dip(&self, dip_w: u32, dip_h: u32, device: (u32, u32)) {
        if dip_w == 0 || dip_h == 0 {
            return;
        }
        self.size.set(device);
        {
            let mut fs = self.frame_state.lock().unwrap();
            fs.dip_w = dip_w;
            fs.dip_h = dip_h;
        }
        self.layout_armed.set(false);
        // The first layout has no bitmap to be wrong about. Later ones do,
        // until a paint arrives at this device size.
        if self.current_frame.borrow().is_some() {
            self.layout_due.set(Some((device, Instant::now())));
        }
        if let Some(host) = self.browser.host() {
            host.was_resized();
        }
    }

    fn set_frame_rate(&self, rate: i32) {
        if self.frame_rate.get() == rate {
            return;
        }
        self.frame_rate.set(rate);
        if let Some(host) = self.browser.host() {
            host.set_windowless_frame_rate(rate);
        }
    }

    pub fn render_frame(&self) -> Option<Arc<gpui::RenderImage>> {
        if self.current_frame.borrow().is_none() {
            let _ = self.update_frame();
        }
        self.current_frame.borrow().clone()
    }

    pub fn take_dropped_frames(&self) -> Vec<Arc<gpui::RenderImage>> {
        std::mem::take(&mut *self.dropped_frames.borrow_mut())
    }

    pub fn current_frame(&self) -> Option<Arc<gpui::RenderImage>> {
        self.current_frame.borrow().clone()
    }

    pub fn update_frame(&self) -> bool {
        let mut fs = self.frame_state.lock().unwrap();
        if fs.has_new && !fs.buffer.is_empty() {
            fs.has_new = false;
            let painted = (fs.buf_w, fs.buf_h);
            if let Some((size, at)) = self.layout_due.get() {
                self.layout_due
                    .set(settle_paint(Some(size), painted).map(|size| (size, at)));
            }
            let (w, h) = painted;
            if let Some(rgba) = image::RgbaImage::from_raw(w, h, fs.buffer.clone()) {
                let render_img = Arc::new(gpui::RenderImage::new(vec![image::Frame::new(rgba)]));
                if let Some(old) = self.current_frame.borrow_mut().replace(render_img) {
                    self.dropped_frames.borrow_mut().push(old);
                }
                return true;
            }
        }
        false
    }

    pub fn read_frame_png(&self) -> Result<Vec<u8>, String> {
        let start_gen = self.frame_state.lock().unwrap().generation;
        let mut saw_loading = false;
        for i in 0..120 {
            self.spin();
            let is_loading = self.shared_state.lock().unwrap().loading;
            if is_loading {
                saw_loading = true;
            }
            let (cur_gen, has_buf) = {
                let fs = self.frame_state.lock().unwrap();
                (fs.generation, !fs.buffer.is_empty())
            };
            let ready = has_buf
                && ((!is_loading && (cur_gen > start_gen || (i >= 15 && saw_loading) || i >= 40))
                    || (cur_gen > start_gen && i >= 20)
                    || i >= 80);
            if ready {
                let fs = self.frame_state.lock().unwrap();
                let mut img = image::RgbaImage::from_raw(fs.buf_w, fs.buf_h, fs.buffer.clone())
                    .ok_or("invalid buffer")?;
                // Buffer is BGRA, swap B and R for correct PNG colors:
                for px in img.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
                drop(fs);
                self.update_frame();
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::DynamicImage::ImageRgba8(img)
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .map_err(|e| e.to_string())?;
                return Ok(bytes.into_inner());
            }
            if let Some(host) = self.browser.host() {
                host.was_hidden(0);
                host.invalidate(PaintElementType::VIEW);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err("no frame rendered".into())
    }

    fn rel_point(&self, pos: Point<Pixels>) -> (i32, i32) {
        let origin = self.origin.get();
        let rel_x = f32::from(pos.x - origin.x).max(0.0);
        let rel_y = f32::from(pos.y - origin.y).max(0.0);
        (rel_x.round() as i32, rel_y.round() as i32)
    }

    pub fn handle_mouse_down(
        &self,
        pos: Point<Pixels>,
        button: gpui::MouseButton,
        modifiers: gpui::Modifiers,
        click_count: usize,
    ) {
        let btn = match button {
            gpui::MouseButton::Left => MouseButtonType::LEFT,
            gpui::MouseButton::Middle => MouseButtonType::MIDDLE,
            gpui::MouseButton::Right => MouseButtonType::RIGHT,
            gpui::MouseButton::Navigate(_) => return,
        };
        self.pressed_button.set(Some(button));
        self.set_focus(true);
        let (x, y) = self.rel_point(pos);
        let event = MouseEvent {
            x,
            y,
            modifiers: cef_mouse_modifiers(&modifiers, None),
        };
        let clicks = click_count.clamp(1, 3) as i32;
        if let Some(host) = self.browser.host() {
            host.send_mouse_click_event(Some(&event), btn, 0, clicks);
        }
        self.spin_soon();
    }

    pub fn handle_mouse_up(
        &self,
        pos: Point<Pixels>,
        button: gpui::MouseButton,
        modifiers: gpui::Modifiers,
        click_count: usize,
    ) {
        let btn = match button {
            gpui::MouseButton::Left => MouseButtonType::LEFT,
            gpui::MouseButton::Middle => MouseButtonType::MIDDLE,
            gpui::MouseButton::Right => MouseButtonType::RIGHT,
            gpui::MouseButton::Navigate(_) => return,
        };
        if self.pressed_button.get() == Some(button) {
            self.pressed_button.set(None);
        }
        let (x, y) = self.rel_point(pos);
        let event = MouseEvent {
            x,
            y,
            modifiers: cef_mouse_modifiers(&modifiers, None),
        };
        let clicks = click_count.clamp(1, 3) as i32;
        if let Some(host) = self.browser.host() {
            host.send_mouse_click_event(Some(&event), btn, 1, clicks);
        }
        self.spin_soon();
    }

    pub fn handle_mouse_up_outside(
        &self,
        pos: Point<Pixels>,
        button: gpui::MouseButton,
        modifiers: gpui::Modifiers,
        click_count: usize,
    ) {
        if self.pressed_button.get() == Some(button) {
            self.handle_mouse_up(pos, button, modifiers, click_count);
        }
    }

    pub fn handle_mouse_move(
        &self,
        pos: Point<Pixels>,
        modifiers: gpui::Modifiers,
        pressed_button: Option<gpui::MouseButton>,
    ) {
        let (x, y) = self.rel_point(pos);
        let event = MouseEvent {
            x,
            y,
            modifiers: cef_mouse_modifiers(&modifiers, pressed_button),
        };
        if let Some(host) = self.browser.host() {
            host.send_mouse_move_event(Some(&event), 0);
        }
        self.spin_soon();
    }

    pub fn handle_mouse_leave(&self, pos: Point<Pixels>, modifiers: gpui::Modifiers) {
        let (x, y) = self.rel_point(pos);
        let event = MouseEvent {
            x,
            y,
            modifiers: cef_mouse_modifiers(&modifiers, None),
        };
        if let Some(host) = self.browser.host() {
            host.send_mouse_move_event(Some(&event), 1);
        }
        self.spin_soon();
    }

    pub fn handle_scroll_wheel(
        &self,
        pos: Point<Pixels>,
        delta: gpui::ScrollDelta,
        modifiers: gpui::Modifiers,
    ) {
        let (x, y) = self.rel_point(pos);
        let mut flags = cef_mouse_modifiers(&modifiers, None);
        if matches!(delta, gpui::ScrollDelta::Pixels(_)) {
            flags |= EVENTFLAG_PRECISION_SCROLLING_DELTA;
        }
        let event = MouseEvent {
            x,
            y,
            modifiers: flags,
        };
        let (dx, dy) = match delta {
            gpui::ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            gpui::ScrollDelta::Lines(p) => (p.x * 40.0, p.y * 40.0),
        };
        if let Some(host) = self.browser.host() {
            host.send_mouse_wheel_event(Some(&event), dx.round() as i32, dy.round() as i32);
        }
        self.spin_soon();
    }

    pub fn handle_key_down(&self, event: &gpui::KeyDownEvent) -> bool {
        match translate_key_down(&event.keystroke, event.is_held) {
            TranslatedKeyDown::Forward => false,
            TranslatedKeyDown::Editor(op) => {
                self.defer_edit(op);
                true
            }
            TranslatedKeyDown::CommitText(text) => {
                self.defer_edit(EditOp::CommitText(text));
                true
            }
            TranslatedKeyDown::Keys { raw_down, char_event } => {
                self.focused.set(true);
                // Not now: `send_key_event` walks the menu bar, and validating a
                // menu item borrows the UI again. That abort is the crash.
                self.defer_key(raw_down, char_event, true);
                true
            }
        }
    }

    pub fn handle_key_up(&self, event: &gpui::KeyUpEvent) -> bool {
        let Some(key_event) = translate_key_up(&event.keystroke) else {
            return false;
        };
        self.defer_key(key_event, None, self.focused.get());
        true
    }

    pub fn set_focus(&self, focused: bool) {
        if self.focused.get() == focused {
            return;
        }
        self.focused.set(focused);
        if let Some(host) = self.browser.host() {
            host.set_focus(if focused { 1 } else { 0 });
        }
        self.spin_soon();
    }

    pub fn focus_parent(&self) {
        self.set_focus(false);
    }

    pub fn copy(&self) {
        self.defer_edit(EditOp::Copy);
    }

    pub fn cut(&self) {
        self.defer_edit(EditOp::Cut);
    }

    pub fn paste(&self) {
        self.defer_edit(EditOp::Paste);
    }

    pub fn paste_and_match_style(&self) {
        self.defer_edit(EditOp::PasteAndMatchStyle);
    }

    pub fn select_all(&self) {
        self.defer_edit(EditOp::SelectAll);
    }

    pub fn undo(&self) {
        self.defer_edit(EditOp::Undo);
    }

    pub fn redo(&self) {
        self.defer_edit(EditOp::Redo);
    }

    pub fn commit_text(&self, text: &str) {
        if !text.is_empty() {
            self.defer_edit(EditOp::CommitText(text.to_string()));
        }
    }

    pub fn spin(&self) {
        #[cfg(target_os = "macos")]
        mac_dispatch::pump();
        #[cfg(not(target_os = "macos"))]
        do_message_loop_work();
    }

    /// Schedule one pump after this UI turn. Safe to call while gpui still
    /// holds the window borrow; `spin` is not.
    pub fn spin_soon(&self) {
        #[cfg(target_os = "macos")]
        mac_dispatch::schedule_work(1);
        #[cfg(not(target_os = "macos"))]
        self.spin();
    }

    /// Deliver a key after this turn. See `handle_key_down`.
    fn defer_key(&self, event: KeyEvent, char_event: Option<KeyEvent>, ensure_focus: bool) {
        #[cfg(target_os = "macos")]
        {
            struct Job {
                browser: Browser,
                event: KeyEvent,
                char_event: Option<KeyEvent>,
                ensure_focus: bool,
            }
            extern "C" fn fire(ctx: *mut std::ffi::c_void) {
                let job = unsafe { Box::from_raw(ctx as *mut Job) };
                if let Some(host) = job.browser.host() {
                    if job.ensure_focus {
                        host.set_focus(1);
                    }
                    host.send_key_event(Some(&job.event));
                    if let Some(ch) = job.char_event.as_ref() {
                        host.send_key_event(Some(ch));
                    }
                }
                mac_dispatch::pump();
            }
            let job = Box::new(Job {
                browser: self.browser.clone(),
                event,
                char_event,
                ensure_focus,
            });
            mac_dispatch::after(Box::into_raw(job) as *mut std::ffi::c_void, fire);
        }
        #[cfg(not(target_os = "macos"))]
        {
            if let Some(host) = self.browser.host() {
                if ensure_focus {
                    host.set_focus(1);
                }
                host.send_key_event(Some(&event));
                if let Some(ch) = char_event.as_ref() {
                    host.send_key_event(Some(ch));
                }
            }
            self.spin();
        }
    }

    fn defer_edit(&self, op: EditOp) {
        self.focused.set(true);
        let cdp_id_base = match &op {
            EditOp::KeyCommand(_) => self.next_cdp_id.fetch_add(2, Ordering::SeqCst),
            _ => 0,
        };
        #[cfg(target_os = "macos")]
        {
            struct Job {
                browser: Browser,
                op: EditOp,
                cdp_id_base: u64,
            }
            extern "C" fn fire(ctx: *mut std::ffi::c_void) {
                let job = unsafe { Box::from_raw(ctx as *mut Job) };
                run_edit_op(&job.browser, job.op, job.cdp_id_base);
                mac_dispatch::pump();
            }
            let job = Box::new(Job {
                browser: self.browser.clone(),
                op,
                cdp_id_base,
            });
            mac_dispatch::after(Box::into_raw(job) as *mut std::ffi::c_void, fire);
        }
        #[cfg(not(target_os = "macos"))]
        {
            run_edit_op(&self.browser, op, cdp_id_base);
            self.spin();
        }
    }

    fn send_cdp<F>(&self, method: &str, params: serde_json::Value, cb: F)
    where
        F: FnOnce(Result<serde_json::Value, String>) + Send + 'static,
    {
        let id = self.next_cdp_id.fetch_add(1, Ordering::SeqCst);
        self.cdp_callbacks.lock().unwrap().insert(id, Box::new(cb));
        let msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        });
        let s = msg.to_string();
        if let Some(host) = self.browser.host() {
            host.send_dev_tools_message(Some(s.as_bytes()));
        }
        self.spin_soon();
    }

    pub fn eval_agent(&self, seq: u64, code: &str) {
        let expr = format!(
            "(function() {{ if (!window.__keelAgent) {{ {} }}; return window.__keelAgent.{}; }})()",
            AGENT_JS, code
        );
        let tx = self.tx.clone();
        self.send_cdp(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": expr,
                "returnByValue": true,
                "awaitPromise": true
            }),
            move |res| {
                let result = match res {
                    Ok(val) => {
                        let v = val.get("result").and_then(|r| r.get("value"));
                        match v {
                            Some(serde_json::Value::String(s)) => Ok(s.clone()),
                            Some(other) => Ok(other.to_string()),
                            None => {
                                if let Some(exc) = val.get("exceptionDetails") {
                                    Err(exc["text"].as_str().unwrap_or("JS exception").to_string())
                                } else {
                                    Ok(String::new())
                                }
                            }
                        }
                    }
                    Err(e) => Err(e),
                };
                let _ = tx.send(NativeEvent::Eval(seq, result));
            },
        );
    }

    pub fn eval_script(&self, seq: u64, script: &str) {
        let expr = format!("(function() {{ return {}; }})()", script);
        let tx = self.tx.clone();
        self.send_cdp(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": expr,
                "returnByValue": true,
                "awaitPromise": true
            }),
            move |res| {
                let result = match res {
                    Ok(val) => {
                        let v = val.get("result").and_then(|r| r.get("value"));
                        match v {
                            Some(serde_json::Value::String(s)) => Ok(s.clone()),
                            Some(other) => Ok(other.to_string()),
                            None => Ok(String::new()),
                        }
                    }
                    Err(e) => Err(e),
                };
                let _ = tx.send(NativeEvent::Eval(seq, result));
            },
        );
    }

    pub fn request_screenshot(&self, full_page: bool) {
        let tx = self.tx.clone();
        let fs = self.frame_state.lock().unwrap();
        let width = if fs.buf_w > 0 { fs.buf_w } else { fs.dip_w };
        let params = if full_page {
            serde_json::json!({
                "format": "png",
                "captureBeyondViewport": true
            })
        } else {
            serde_json::json!({
                "format": "png"
            })
        };
        self.send_cdp("Page.captureScreenshot", params, move |res| {
            let outcome = match res {
                Ok(val) => {
                    if let Some(b64) = val.get("data").and_then(|d| d.as_str()) {
                        match base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
                            Ok(bytes) => {
                                let w = if let Ok(img) = image::load_from_memory_with_format(
                                    &bytes,
                                    image::ImageFormat::Png,
                                ) {
                                    img.width()
                                } else {
                                    width
                                };
                                Ok((bytes, w))
                            }
                            Err(e) => Err(format!("failed to decode screenshot base64: {e}")),
                        }
                    } else {
                        Err("no screenshot data in CDP response".to_string())
                    }
                }
                Err(e) => Err(e),
            };
            let _ = tx.send(NativeEvent::Screenshot(outcome));
        });
    }

    pub fn request_page_size(&self) {
        let tx = self.tx.clone();
        self.send_cdp(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": PAGE_SIZE_JS,
                "returnByValue": true,
                "awaitPromise": true
            }),
            move |res| {
                let result = match res {
                    Ok(val) => val.get("result").and_then(|r| r.get("value")).map_or_else(
                        || Err("no page size returned".to_string()),
                        |v| {
                            Ok(v.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| v.to_string()))
                        },
                    ),
                    Err(e) => Err(e),
                };
                let _ = tx.send(NativeEvent::PageSize(result));
            },
        );
    }

    pub fn request_frame_scroll(&self, y: f64) {
        let code = format!("({})({})", FRAME_JS, y);
        let tx = self.tx.clone();
        self.send_cdp(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": code,
                "returnByValue": true,
                "awaitPromise": true
            }),
            move |res| {
                let result = match res {
                    Ok(val) => val.get("result").and_then(|r| r.get("value")).map_or_else(
                        || Err("scroll failed".to_string()),
                        |v| {
                            Ok(v.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| v.to_string()))
                        },
                    ),
                    Err(e) => Err(e),
                };
                let _ = tx.send(NativeEvent::Scrolled(result));
            },
        );
    }

    pub fn finish_full_page(&self, sx: f64, sy: f64) {
        let code = format!("({})({}, {})", RESTORE_JS, sx, sy);
        self.send_cdp(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": code
            }),
            |_| {},
        );
    }

    pub fn hide_fixed(&self) {
        self.send_cdp(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": HIDE_FIXED_JS
            }),
            |_| {},
        );
    }

    pub fn history(&self, forward: bool) {
        if forward {
            self.go_forward();
        } else {
            self.go_back();
        }
    }

    pub fn set_shown(&self, shown: bool) {
        self.shown.set(shown);
        if shown {
            self.unpark();
        } else if !self.background.get() {
            self.park();
        }
    }

    pub fn has_dialog(&self) -> bool {
        self.held_dialog.lock().unwrap().is_some()
    }

    pub fn answer_dialog(&self, accept: bool, text: Option<&str>) -> Option<DialogKind> {
        let (kind, cb) = self.held_dialog.lock().unwrap().take()?;
        let text_str = text.map(CefString::from);
        cb.cont(if accept { 1 } else { 0 }, text_str.as_ref());
        Some(kind)
    }

    pub fn set_upload(&self, files: Option<Vec<PathBuf>>) {
        self.shared_state.lock().unwrap().upload = files;
    }

    pub fn park(&self) {
        self.parked.set(true);
        self.focused.set(false);
        if let Some(host) = self.browser.host() {
            host.set_focus(0);
            host.was_hidden(1);
        }
        self.set_frame_rate(1);
    }

    pub fn unpark(&self) {
        self.parked.set(false);
        if let Some(host) = self.browser.host() {
            host.was_hidden(0);
        }
        self.set_frame_rate(60);
    }

    pub fn is_parked(&self) -> bool {
        self.parked.get()
    }

    pub fn debug_info(&self) -> serde_json::Value {
        let fs = self.frame_state.lock().unwrap();
        let ss = self.shared_state.lock().unwrap();
        let main_frame = self.browser.main_frame();
        serde_json::json!({
            "has_main_frame": main_frame.is_some(),
            "main_frame_url": main_frame.as_ref().map(|f| CefString::from(&f.url()).to_string()),
            "frame_count": self.browser.frame_count(),
            "has_document": self.browser.has_document() != 0,
            "is_loading": self.browser.is_loading() != 0,
            "shared_url": ss.url,
            "shared_title": ss.title,
            "shared_loading": ss.loading,
            "shared_progress": ss.progress,
            "frame_w": fs.buf_w,
            "frame_h": fs.buf_h,
            "dip_w": fs.dip_w,
            "dip_h": fs.dip_h,
            "frame_scale": fs.scale,
            "has_new": fs.has_new,
            "buffer_len": fs.buffer.len(),
            "is_parked": self.is_parked(),
            "shown": self.shown.get(),
            "background": self.background.get(),
        })
    }

    pub fn send_cdp_sync(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout_ms: u64,
    ) -> Result<serde_json::Value, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.send_cdp(method, params, move |res| {
            let _ = tx.send(res);
        });
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(timeout_ms) {
            self.spin();
            if let Ok(res) = rx.try_recv() {
                return res;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err("CDP request timed out".to_string())
    }

    pub fn eval_agent_sync(&self, code: &str, timeout_ms: u64) -> Result<String, String> {
        let expr = format!(
            "(function() {{ if (!window.__keelAgent) {{ {} }}; return window.__keelAgent.{}; }})()",
            AGENT_JS, code
        );
        let res = self.send_cdp_sync(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": expr,
                "returnByValue": true,
                "awaitPromise": true
            }),
            timeout_ms,
        )?;
        let v = res.get("result").and_then(|r| r.get("value"));
        match v {
            Some(serde_json::Value::String(s)) => Ok(s.clone()),
            Some(other) => Ok(other.to_string()),
            None => {
                if let Some(exc) = res.get("exceptionDetails") {
                    Err(exc["text"].as_str().unwrap_or("JS exception").to_string())
                } else {
                    Ok(String::new())
                }
            }
        }
    }

    pub fn screenshot_sync(&self, full_page: bool, timeout_ms: u64) -> Result<Vec<u8>, String> {
        let params = if full_page {
            serde_json::json!({
                "format": "png",
                "captureBeyondViewport": true
            })
        } else {
            serde_json::json!({
                "format": "png"
            })
        };
        let res = self.send_cdp_sync("Page.captureScreenshot", params, timeout_ms)?;
        if let Some(b64) = res.get("data").and_then(|d| d.as_str()) {
            base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|e| format!("failed to decode screenshot base64: {e}"))
        } else {
            Err("no screenshot data in CDP response".to_string())
        }
    }
}

impl Drop for NativePage {
    fn drop(&mut self) {
        if let Some(host) = self.browser.host() {
            host.close_browser(1);
        }
    }
}

// `cef_event_flags_t` bit values from `include/internal/cef_types.h`.
const EVENTFLAG_SHIFT_DOWN: u32 = 1 << 1;
const EVENTFLAG_CONTROL_DOWN: u32 = 1 << 2;
const EVENTFLAG_ALT_DOWN: u32 = 1 << 3;
const EVENTFLAG_LEFT_MOUSE_BUTTON: u32 = 1 << 4;
const EVENTFLAG_MIDDLE_MOUSE_BUTTON: u32 = 1 << 5;
const EVENTFLAG_RIGHT_MOUSE_BUTTON: u32 = 1 << 6;
const EVENTFLAG_COMMAND_DOWN: u32 = 1 << 7;
const EVENTFLAG_IS_KEY_PAD: u32 = 1 << 9;
const EVENTFLAG_IS_REPEAT: u32 = 1 << 13;
const EVENTFLAG_PRECISION_SCROLLING_DELTA: u32 = 1 << 14;

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyCommandSpec {
    key: &'static str,
    code: &'static str,
    windows_key_code: i32,
    native_key_code: i32,
    cdp_modifiers: u32,
    is_repeat: bool,
    command: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditOp {
    Copy,
    Cut,
    Paste,
    PasteAndMatchStyle,
    SelectAll,
    Undo,
    Redo,
    CommitText(String),
    KeyCommand(KeyCommandSpec),
}

fn run_edit_op(browser: &Browser, op: EditOp, cdp_id_base: u64) {
    if let Some(host) = browser.host() {
        host.set_focus(1);
        match op {
            EditOp::CommitText(text) => {
                let cef_text = CefString::from(text.as_str());
                let invalid = Range {
                    from: u32::MAX,
                    to: u32::MAX,
                };
                host.ime_commit_text(Some(&cef_text), Some(&invalid), 0);
                return;
            }
            EditOp::KeyCommand(spec) => {
                let down = serde_json::json!({
                    "id": cdp_id_base,
                    "method": "Input.dispatchKeyEvent",
                    "params": {
                        "type": "rawKeyDown",
                        "modifiers": spec.cdp_modifiers,
                        "windowsVirtualKeyCode": spec.windows_key_code,
                        "nativeVirtualKeyCode": spec.native_key_code,
                        "key": spec.key,
                        "code": spec.code,
                        "autoRepeat": spec.is_repeat,
                        "commands": [spec.command],
                    }
                })
                .to_string();
                let up = serde_json::json!({
                    "id": cdp_id_base + 1,
                    "method": "Input.dispatchKeyEvent",
                    "params": {
                        "type": "keyUp",
                        "modifiers": spec.cdp_modifiers,
                        "windowsVirtualKeyCode": spec.windows_key_code,
                        "nativeVirtualKeyCode": spec.native_key_code,
                        "key": spec.key,
                        "code": spec.code,
                    }
                })
                .to_string();
                host.send_dev_tools_message(Some(down.as_bytes()));
                host.send_dev_tools_message(Some(up.as_bytes()));
                return;
            }
            _ => {}
        }
    }
    let Some(frame) = browser.focused_frame().or_else(|| browser.main_frame()) else {
        return;
    };
    match op {
        EditOp::Copy => frame.copy(),
        EditOp::Cut => frame.cut(),
        EditOp::Paste => frame.paste(),
        EditOp::PasteAndMatchStyle => frame.paste_and_match_style(),
        EditOp::SelectAll => frame.select_all(),
        EditOp::Undo => frame.undo(),
        EditOp::Redo => frame.redo(),
        EditOp::CommitText(_) | EditOp::KeyCommand(_) => {}
    }
}

fn cef_mouse_modifiers(
    mods: &gpui::Modifiers,
    pressed_button: Option<gpui::MouseButton>,
) -> u32 {
    let mut flags = 0u32;
    if mods.shift {
        flags |= EVENTFLAG_SHIFT_DOWN;
    }
    if mods.control {
        flags |= EVENTFLAG_CONTROL_DOWN;
    }
    if mods.alt {
        flags |= EVENTFLAG_ALT_DOWN;
    }
    if mods.platform {
        flags |= EVENTFLAG_COMMAND_DOWN;
    }
    match pressed_button {
        Some(gpui::MouseButton::Left) => flags |= EVENTFLAG_LEFT_MOUSE_BUTTON,
        Some(gpui::MouseButton::Middle) => flags |= EVENTFLAG_MIDDLE_MOUSE_BUTTON,
        Some(gpui::MouseButton::Right) => flags |= EVENTFLAG_RIGHT_MOUSE_BUTTON,
        _ => {}
    }
    flags
}

fn cef_key_modifiers(
    mods: &gpui::Modifiers,
    inferred_shift: bool,
    is_held: bool,
    is_keypad: bool,
) -> u32 {
    let mut flags = 0u32;
    if mods.shift || inferred_shift {
        flags |= EVENTFLAG_SHIFT_DOWN;
    }
    if mods.control {
        flags |= EVENTFLAG_CONTROL_DOWN;
    }
    if mods.alt {
        flags |= EVENTFLAG_ALT_DOWN;
    }
    if mods.platform {
        flags |= EVENTFLAG_COMMAND_DOWN;
    }
    if is_keypad {
        flags |= EVENTFLAG_IS_KEY_PAD;
    }
    if is_held {
        flags |= EVENTFLAG_IS_REPEAT;
    }
    flags
}

fn cef_cursor_to_gpui(cursor: CursorType) -> gpui::CursorStyle {
    use gpui::CursorStyle;
    if cursor == CursorType::IBEAM {
        CursorStyle::IBeam
    } else if cursor == CursorType::VERTICALTEXT {
        CursorStyle::IBeamCursorForVerticalLayout
    } else if cursor == CursorType::HAND {
        CursorStyle::PointingHand
    } else if cursor == CursorType::CROSS || cursor == CursorType::CELL {
        CursorStyle::Crosshair
    } else if cursor == CursorType::GRAB {
        CursorStyle::OpenHand
    } else if cursor == CursorType::GRABBING
        || cursor == CursorType::MOVE
        || cursor == CursorType::DND_MOVE
    {
        CursorStyle::ClosedHand
    } else if cursor == CursorType::WESTRESIZE {
        CursorStyle::ResizeLeft
    } else if cursor == CursorType::EASTRESIZE {
        CursorStyle::ResizeRight
    } else if cursor == CursorType::EASTWESTRESIZE {
        CursorStyle::ResizeLeftRight
    } else if cursor == CursorType::NORTHRESIZE {
        CursorStyle::ResizeUp
    } else if cursor == CursorType::SOUTHRESIZE {
        CursorStyle::ResizeDown
    } else if cursor == CursorType::NORTHSOUTHRESIZE {
        CursorStyle::ResizeUpDown
    } else if cursor == CursorType::COLUMNRESIZE {
        CursorStyle::ResizeColumn
    } else if cursor == CursorType::ROWRESIZE {
        CursorStyle::ResizeRow
    } else if cursor == CursorType::NORTHWESTRESIZE
        || cursor == CursorType::SOUTHEASTRESIZE
        || cursor == CursorType::NORTHWESTSOUTHEASTRESIZE
    {
        CursorStyle::ResizeUpLeftDownRight
    } else if cursor == CursorType::NORTHEASTRESIZE
        || cursor == CursorType::SOUTHWESTRESIZE
        || cursor == CursorType::NORTHEASTSOUTHWESTRESIZE
    {
        CursorStyle::ResizeUpRightDownLeft
    } else if cursor == CursorType::NOTALLOWED
        || cursor == CursorType::NODROP
        || cursor == CursorType::DND_NONE
    {
        CursorStyle::OperationNotAllowed
    } else if cursor == CursorType::ALIAS || cursor == CursorType::DND_LINK {
        CursorStyle::DragLink
    } else if cursor == CursorType::COPY || cursor == CursorType::DND_COPY {
        CursorStyle::DragCopy
    } else if cursor == CursorType::CONTEXTMENU {
        CursorStyle::ContextualMenu
    } else {
        CursorStyle::Arrow
    }
}

#[derive(Debug)]
enum TranslatedKeyDown {
    Forward,
    Editor(EditOp),
    CommitText(String),
    Keys {
        raw_down: KeyEvent,
        char_event: Option<KeyEvent>,
    },
}

struct KeySpec {
    windows_key_code: i32,
    native_key_code: i32,
    character: u16,
    unmodified_character: u16,
    emit_char: bool,
    is_keypad: bool,
    inferred_shift: bool,
}

fn translate_key_down(ks: &gpui::Keystroke, is_held: bool) -> TranslatedKeyDown {
    if forwarded_key(ks.modifiers.platform, ks.modifiers.control, &ks.key) {
        return TranslatedKeyDown::Forward;
    }

    // Standard macOS Cmd editing commands run directly on the focused CEF frame
    // because windowless CEF has no Cocoa Edit menu responder chain.
    if ks.modifiers.platform && !ks.modifiers.control {
        let lower = ks.key.to_ascii_lowercase();
        match (lower.as_str(), ks.modifiers.shift, ks.modifiers.alt) {
            ("c", false, false) => return TranslatedKeyDown::Editor(EditOp::Copy),
            ("x", false, false) => return TranslatedKeyDown::Editor(EditOp::Cut),
            ("v", false, false) => return TranslatedKeyDown::Editor(EditOp::Paste),
            ("v", true, false) | ("v", true, true) => {
                return TranslatedKeyDown::Editor(EditOp::PasteAndMatchStyle);
            }
            ("a", false, false) => return TranslatedKeyDown::Editor(EditOp::SelectAll),
            ("z", false, false) => return TranslatedKeyDown::Editor(EditOp::Undo),
            ("z", true, false) | ("y", false, false) => {
                return TranslatedKeyDown::Editor(EditOp::Redo);
            }
            _ => {}
        }
    }

    if let Some(cmd_spec) = mac_key_command(ks, is_held) {
        return TranslatedKeyDown::Editor(EditOp::KeyCommand(cmd_spec));
    }

    if !ks.modifiers.platform
        && !ks.modifiers.control
        && let Some(text) = ks.key_char.as_deref()
        && text.encode_utf16().count() > 1
    {
        return TranslatedKeyDown::CommitText(text.to_string());
    }

    let spec = key_spec(&ks.key, ks.key_char.as_deref(), &ks.modifiers);
    let modifiers = cef_key_modifiers(
        &ks.modifiers,
        spec.inferred_shift,
        is_held,
        spec.is_keypad,
    );
    let raw_down = KeyEvent {
        size: std::mem::size_of::<sys::_cef_key_event_t>(),
        type_: KeyEventType::RAWKEYDOWN,
        modifiers,
        windows_key_code: spec.windows_key_code,
        native_key_code: spec.native_key_code,
        is_system_key: 0,
        character: spec.character,
        unmodified_character: spec.unmodified_character,
        focus_on_editable_field: 0,
    };
    let char_event = spec.emit_char.then(|| KeyEvent {
        type_: KeyEventType::CHAR,
        windows_key_code: i32::from(spec.character),
        ..raw_down
    });
    TranslatedKeyDown::Keys {
        raw_down,
        char_event,
    }
}

fn translate_key_up(ks: &gpui::Keystroke) -> Option<KeyEvent> {
    if forwarded_key(ks.modifiers.platform, ks.modifiers.control, &ks.key) {
        return None;
    }
    if mac_key_command(ks, false).is_some() {
        return None;
    }
    let spec = key_spec(&ks.key, ks.key_char.as_deref(), &ks.modifiers);
    let modifiers = cef_key_modifiers(&ks.modifiers, spec.inferred_shift, false, spec.is_keypad);
    Some(KeyEvent {
        size: std::mem::size_of::<sys::_cef_key_event_t>(),
        type_: KeyEventType::KEYUP,
        modifiers,
        windows_key_code: spec.windows_key_code,
        native_key_code: spec.native_key_code,
        is_system_key: 0,
        character: spec.character,
        unmodified_character: spec.unmodified_character,
        focus_on_editable_field: 0,
    })
}

/// Cocoa `NSTextInputContext` line/document/Emacs bindings that windowless CEF
/// on macOS does not synthesize from raw `send_key_event` alone.
fn mac_key_command(ks: &gpui::Keystroke, is_repeat: bool) -> Option<KeyCommandSpec> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (ks, is_repeat);
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        if ks.modifiers.alt {
            return None;
        }
        let lower = ks.key.to_ascii_lowercase();
        let shift_bit = if ks.modifiers.shift { 8u32 } else { 0u32 };
        if ks.modifiers.platform && !ks.modifiers.control {
            let cdp_modifiers = 4u32 | shift_bit;
            let (key, code, windows_key_code, native_key_code, command) =
                match (lower.as_str(), ks.modifiers.shift) {
                    ("backspace", false) => {
                        ("Backspace", "Backspace", 0x08, 0x33, "deleteToBeginningOfLine")
                    }
                    ("delete", false) => ("Delete", "Delete", 0x2E, 0x75, "deleteToEndOfLine"),
                    ("left" | "arrowleft", false) => {
                        ("ArrowLeft", "ArrowLeft", 0x25, 0x7B, "moveToBeginningOfLine")
                    }
                    ("left" | "arrowleft", true) => (
                        "ArrowLeft",
                        "ArrowLeft",
                        0x25,
                        0x7B,
                        "moveToBeginningOfLineAndModifySelection",
                    ),
                    ("right" | "arrowright", false) => {
                        ("ArrowRight", "ArrowRight", 0x27, 0x7C, "moveToEndOfLine")
                    }
                    ("right" | "arrowright", true) => (
                        "ArrowRight",
                        "ArrowRight",
                        0x27,
                        0x7C,
                        "moveToEndOfLineAndModifySelection",
                    ),
                    ("up" | "arrowup", false) => (
                        "ArrowUp",
                        "ArrowUp",
                        0x26,
                        0x7E,
                        "moveToBeginningOfDocument",
                    ),
                    ("up" | "arrowup", true) => (
                        "ArrowUp",
                        "ArrowUp",
                        0x26,
                        0x7E,
                        "moveToBeginningOfDocumentAndModifySelection",
                    ),
                    ("down" | "arrowdown", false) => (
                        "ArrowDown",
                        "ArrowDown",
                        0x28,
                        0x7D,
                        "moveToEndOfDocument",
                    ),
                    ("down" | "arrowdown", true) => (
                        "ArrowDown",
                        "ArrowDown",
                        0x28,
                        0x7D,
                        "moveToEndOfDocumentAndModifySelection",
                    ),
                    _ => return None,
                };
            return Some(KeyCommandSpec {
                key,
                code,
                windows_key_code,
                native_key_code,
                cdp_modifiers,
                is_repeat,
                command,
            });
        }
        if ks.modifiers.control && !ks.modifiers.platform {
            let cdp_modifiers = 2u32 | shift_bit;
            let (key, code, windows_key_code, native_key_code, command) =
                match (lower.as_str(), ks.modifiers.shift) {
                    ("a", false) => ("a", "KeyA", 0x41, 0x00, "moveToBeginningOfParagraph"),
                    ("a", true) => (
                        "A",
                        "KeyA",
                        0x41,
                        0x00,
                        "moveToBeginningOfParagraphAndModifySelection",
                    ),
                    ("e", false) => ("e", "KeyE", 0x45, 0x0E, "moveToEndOfParagraph"),
                    ("e", true) => (
                        "E",
                        "KeyE",
                        0x45,
                        0x0E,
                        "moveToEndOfParagraphAndModifySelection",
                    ),
                    ("k", false) => ("k", "KeyK", 0x4B, 0x28, "deleteToEndOfParagraph"),
                    ("d", false) => ("d", "KeyD", 0x44, 0x02, "deleteForward"),
                    ("h", false) => ("h", "KeyH", 0x48, 0x04, "deleteBackward"),
                    _ => return None,
                };
            return Some(KeyCommandSpec {
                key,
                code,
                windows_key_code,
                native_key_code,
                cdp_modifiers,
                is_repeat,
                command,
            });
        }
        None
    }
}

fn key_spec(key: &str, key_char: Option<&str>, mods: &gpui::Modifiers) -> KeySpec {
    let lower = key.to_ascii_lowercase();
    let plain_text_mods = !mods.platform && !mods.control;

    match lower.as_str() {
        "backspace" => KeySpec {
            windows_key_code: 0x08,
            native_key_code: 0x33,
            character: 0x7F,
            unmodified_character: 0x7F,
            emit_char: false,
            is_keypad: false,
            inferred_shift: false,
        },
        "delete" => KeySpec {
            windows_key_code: 0x2E,
            native_key_code: 0x75,
            character: 0xF728,
            unmodified_character: 0xF728,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        "enter" => KeySpec {
            windows_key_code: 0x0D,
            native_key_code: 0x24,
            character: 0x0D,
            unmodified_character: 0x0D,
            emit_char: plain_text_mods,
            is_keypad: false,
            inferred_shift: false,
        },
        "tab" => KeySpec {
            windows_key_code: 0x09,
            native_key_code: 0x30,
            character: if mods.shift { 0x19 } else { 0x09 },
            unmodified_character: 0x09,
            emit_char: false,
            is_keypad: false,
            inferred_shift: false,
        },
        "escape" => KeySpec {
            windows_key_code: 0x1B,
            native_key_code: 0x35,
            character: 0x1B,
            unmodified_character: 0x1B,
            emit_char: false,
            is_keypad: false,
            inferred_shift: false,
        },
        "space" => KeySpec {
            windows_key_code: 0x20,
            native_key_code: 0x31,
            character: 0x20,
            unmodified_character: 0x20,
            emit_char: plain_text_mods,
            is_keypad: false,
            inferred_shift: false,
        },
        "up" | "arrowup" => KeySpec {
            windows_key_code: 0x26,
            native_key_code: 0x7E,
            character: 0xF700,
            unmodified_character: 0xF700,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        "down" | "arrowdown" => KeySpec {
            windows_key_code: 0x28,
            native_key_code: 0x7D,
            character: 0xF701,
            unmodified_character: 0xF701,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        "left" | "arrowleft" => KeySpec {
            windows_key_code: 0x25,
            native_key_code: 0x7B,
            character: 0xF702,
            unmodified_character: 0xF702,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        "right" | "arrowright" => KeySpec {
            windows_key_code: 0x27,
            native_key_code: 0x7C,
            character: 0xF703,
            unmodified_character: 0xF703,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        "home" => KeySpec {
            windows_key_code: 0x24,
            native_key_code: 0x73,
            character: 0xF729,
            unmodified_character: 0xF729,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        "end" => KeySpec {
            windows_key_code: 0x23,
            native_key_code: 0x77,
            character: 0xF72B,
            unmodified_character: 0xF72B,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        "pageup" => KeySpec {
            windows_key_code: 0x21,
            native_key_code: 0x74,
            character: 0xF72C,
            unmodified_character: 0xF72C,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        "pagedown" => KeySpec {
            windows_key_code: 0x22,
            native_key_code: 0x79,
            character: 0xF72D,
            unmodified_character: 0xF72D,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        "insert" => KeySpec {
            windows_key_code: 0x2D,
            native_key_code: 0x72,
            character: 0xF746,
            unmodified_character: 0xF746,
            emit_char: false,
            is_keypad: true,
            inferred_shift: false,
        },
        s if s.starts_with('f') && s.len() >= 2 => {
            if let Ok(n) = s[1..].parse::<u8>()
                && (1..=20).contains(&n)
            {
                let f_char = 0xF704u16 + u16::from(n - 1);
                return KeySpec {
                    windows_key_code: 0x70 + i32::from(n - 1),
                    native_key_code: mac_fn_key_code(n),
                    character: f_char,
                    unmodified_character: f_char,
                    emit_char: false,
                    is_keypad: true,
                    inferred_shift: false,
                };
            }
            char_key_spec(key, key_char, mods)
        }
        _ => char_key_spec(key, key_char, mods),
    }
}

fn char_key_spec(key: &str, key_char: Option<&str>, mods: &gpui::Modifiers) -> KeySpec {
    let key_u16 = single_utf16(key);
    let typed_u16 = key_char.and_then(single_utf16);
    let base_ch = key.chars().next().filter(|_| key.chars().count() == 1);
    let (vk, native_key_code, shifted_symbol) = base_ch
        .map(ansi_char_codes)
        .unwrap_or((0, 0xFF, false));

    let unmodified_character = match (base_ch, mods.shift) {
        (Some(c), true) if c.is_ascii_lowercase() => c.to_ascii_uppercase() as u16,
        _ => key_u16.or(typed_u16).unwrap_or(0),
    };

    let character = if mods.control && !mods.platform && !mods.alt {
        if let Some(c) = base_ch {
            let lc = c.to_ascii_lowercase();
            if lc.is_ascii_lowercase() {
                u16::from(lc as u8 - b'a' + 1)
            } else {
                unmodified_character
            }
        } else {
            0
        }
    } else {
        typed_u16.unwrap_or(unmodified_character)
    };

    let emit_char = !mods.platform
        && !mods.control
        && character >= 0x20
        && character != 0x7F
        && !(0xF700..=0xF8FF).contains(&character);

    KeySpec {
        windows_key_code: vk,
        native_key_code,
        character,
        unmodified_character,
        emit_char,
        is_keypad: false,
        // GPUI clears `modifiers.shift` on macOS when Shift produces a non-letter
        // ASCII symbol (`@`, `:`, `_`, etc.). Restore it for accurate DOM key events.
        inferred_shift: shifted_symbol && !mods.platform && !mods.control && !mods.alt,
    }
}

fn single_utf16(s: &str) -> Option<u16> {
    let mut it = s.encode_utf16();
    let first = it.next()?;
    if it.next().is_none() {
        Some(first)
    } else {
        None
    }
}

fn mac_fn_key_code(n: u8) -> i32 {
    match n {
        1 => 0x7A,
        2 => 0x78,
        3 => 0x63,
        4 => 0x76,
        5 => 0x60,
        6 => 0x61,
        7 => 0x62,
        8 => 0x64,
        9 => 0x65,
        10 => 0x6D,
        11 => 0x67,
        12 => 0x6F,
        13 => 0x69,
        14 => 0x6B,
        15 => 0x71,
        16 => 0x6A,
        17 => 0x40,
        18 => 0x4F,
        19 => 0x50,
        20 => 0x5A,
        _ => 0xFF,
    }
}

/// Returns `(windows_key_code, mac_carbon_key_code, is_shifted_us_symbol)`.
fn ansi_char_codes(ch: char) -> (i32, i32, bool) {
    match ch.to_ascii_lowercase() {
        'a' => (0x41, 0x00, false),
        's' => (0x53, 0x01, false),
        'd' => (0x44, 0x02, false),
        'f' => (0x46, 0x03, false),
        'h' => (0x48, 0x04, false),
        'g' => (0x47, 0x05, false),
        'z' => (0x5A, 0x06, false),
        'x' => (0x58, 0x07, false),
        'c' => (0x43, 0x08, false),
        'v' => (0x56, 0x09, false),
        'b' => (0x42, 0x0B, false),
        'q' => (0x51, 0x0C, false),
        'w' => (0x57, 0x0D, false),
        'e' => (0x45, 0x0E, false),
        'r' => (0x52, 0x0F, false),
        'y' => (0x59, 0x10, false),
        't' => (0x54, 0x11, false),
        '1' => (0x31, 0x12, false),
        '!' => (0x31, 0x12, true),
        '2' => (0x32, 0x13, false),
        '@' => (0x32, 0x13, true),
        '3' => (0x33, 0x14, false),
        '#' => (0x33, 0x14, true),
        '4' => (0x34, 0x15, false),
        '$' => (0x34, 0x15, true),
        '6' => (0x36, 0x16, false),
        '^' => (0x36, 0x16, true),
        '5' => (0x35, 0x17, false),
        '%' => (0x35, 0x17, true),
        '=' => (0xBB, 0x18, false),
        '+' => (0xBB, 0x18, true),
        '9' => (0x39, 0x19, false),
        '(' => (0x39, 0x19, true),
        '7' => (0x37, 0x1A, false),
        '&' => (0x37, 0x1A, true),
        '-' => (0xBD, 0x1B, false),
        '_' => (0xBD, 0x1B, true),
        '8' => (0x38, 0x1C, false),
        '*' => (0x38, 0x1C, true),
        '0' => (0x30, 0x1D, false),
        ')' => (0x30, 0x1D, true),
        ']' => (0xDD, 0x1E, false),
        '}' => (0xDD, 0x1E, true),
        'o' => (0x4F, 0x1F, false),
        'u' => (0x55, 0x20, false),
        '[' => (0xDB, 0x21, false),
        '{' => (0xDB, 0x21, true),
        'i' => (0x49, 0x22, false),
        'p' => (0x50, 0x23, false),
        'l' => (0x4C, 0x25, false),
        'j' => (0x4A, 0x26, false),
        '\'' => (0xDE, 0x27, false),
        '"' => (0xDE, 0x27, true),
        'k' => (0x4B, 0x28, false),
        ';' => (0xBA, 0x29, false),
        ':' => (0xBA, 0x29, true),
        '\\' => (0xDC, 0x2A, false),
        '|' => (0xDC, 0x2A, true),
        ',' => (0xBC, 0x2B, false),
        '<' => (0xBC, 0x2B, true),
        '/' => (0xBF, 0x2C, false),
        '?' => (0xBF, 0x2C, true),
        'n' => (0x4E, 0x2D, false),
        'm' => (0x4D, 0x2E, false),
        '.' => (0xBE, 0x2F, false),
        '>' => (0xBE, 0x2F, true),
        '`' => (0xC0, 0x32, false),
        '~' => (0xC0, 0x32, true),
        _ => (0, 0xFF, false),
    }
}

fn urlencoding_simple(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod settle_tests {
    use super::*;
    use gpui::{Keystroke, Modifiers, MouseButton};

    #[test]
    fn only_a_paint_at_the_new_size_catches_up() {
        let due = Some((800, 600));
        assert_eq!(settle_paint(due, (400, 600)), due);
        assert_eq!(settle_paint(due, (800, 600)), None);
        assert_eq!(settle_paint(None, (800, 600)), None);
    }

    #[test]
    fn cef_modifier_flags_match_cef_types_header() {
        let mods = Modifiers {
            shift: true,
            control: true,
            alt: true,
            platform: true,
            function: false,
        };
        let flags = cef_key_modifiers(&mods, false, true, true);
        assert_eq!(
            flags,
            (1 << 1) | (1 << 2) | (1 << 3) | (1 << 7) | (1 << 9) | (1 << 13)
        );
        let drag = cef_mouse_modifiers(&Modifiers::default(), Some(MouseButton::Left));
        assert_eq!(drag, 1 << 4);
    }

    #[test]
    fn backspace_and_delete_use_macos_keycodes_without_char_events() {
        let bs = Keystroke {
            modifiers: Modifiers::default(),
            key: "backspace".into(),
            key_char: None,
        };
        match translate_key_down(&bs, false) {
            TranslatedKeyDown::Keys {
                raw_down,
                char_event,
            } => {
                assert_eq!(raw_down.windows_key_code, 0x08);
                assert_eq!(raw_down.native_key_code, 0x33);
                assert_eq!(raw_down.character, 0x7F);
                assert_eq!(raw_down.unmodified_character, 0x7F);
                assert!(char_event.is_none());
            }
            other => panic!("unexpected backspace translation: {other:?}"),
        }

        let cmd_bs = Keystroke {
            modifiers: Modifiers {
                platform: true,
                ..Default::default()
            },
            key: "backspace".into(),
            key_char: None,
        };
        #[cfg(target_os = "macos")]
        match translate_key_down(&cmd_bs, false) {
            TranslatedKeyDown::Editor(EditOp::KeyCommand(spec)) => {
                assert_eq!(spec.command, "deleteToBeginningOfLine");
                assert_eq!(spec.windows_key_code, 0x08);
                assert_eq!(spec.native_key_code, 0x33);
                assert_eq!(spec.cdp_modifiers, 4);
                assert!(translate_key_up(&cmd_bs).is_none());
            }
            other => panic!("unexpected cmd-backspace translation: {other:?}"),
        }

        let del = Keystroke {
            modifiers: Modifiers::default(),
            key: "delete".into(),
            key_char: None,
        };
        match translate_key_down(&del, false) {
            TranslatedKeyDown::Keys {
                raw_down,
                char_event,
            } => {
                assert_eq!(raw_down.windows_key_code, 0x2E);
                assert_eq!(raw_down.native_key_code, 0x75);
                assert_eq!(raw_down.character, 0xF728);
                assert_ne!(raw_down.modifiers & EVENTFLAG_IS_KEY_PAD, 0);
                assert!(char_event.is_none());
            }
            other => panic!("unexpected delete translation: {other:?}"),
        }
    }

    #[test]
    fn space_enter_uppercase_and_shifted_symbols_emit_proper_chars() {
        let space = Keystroke {
            modifiers: Modifiers::default(),
            key: "space".into(),
            key_char: Some(" ".into()),
        };
        match translate_key_down(&space, false) {
            TranslatedKeyDown::Keys {
                raw_down,
                char_event: Some(ch),
            } => {
                assert_eq!(raw_down.native_key_code, 0x31);
                assert_eq!(ch.character, 0x20);
                assert_eq!(ch.windows_key_code, 0x20);
            }
            other => panic!("unexpected space translation: {other:?}"),
        }

        let enter = Keystroke {
            modifiers: Modifiers::default(),
            key: "enter".into(),
            key_char: Some("\n".into()),
        };
        match translate_key_down(&enter, false) {
            TranslatedKeyDown::Keys {
                raw_down,
                char_event: Some(ch),
            } => {
                assert_eq!(raw_down.native_key_code, 0x24);
                assert_eq!(ch.character, 0x0D);
            }
            other => panic!("unexpected enter translation: {other:?}"),
        }

        let upper_a = Keystroke {
            modifiers: Modifiers {
                shift: true,
                ..Default::default()
            },
            key: "a".into(),
            key_char: Some("A".into()),
        };
        match translate_key_down(&upper_a, false) {
            TranslatedKeyDown::Keys {
                raw_down,
                char_event: Some(ch),
            } => {
                assert_eq!(raw_down.windows_key_code, 0x41);
                assert_eq!(raw_down.native_key_code, 0x00);
                assert_eq!(ch.character, 'A' as u16);
                assert_eq!(ch.windows_key_code, 'A' as i32);
            }
            other => panic!("unexpected Shift+A translation: {other:?}"),
        }

        // GPUI clears `modifiers.shift` for shifted symbols like `@`.
        let at_sign = Keystroke {
            modifiers: Modifiers::default(),
            key: "@".into(),
            key_char: Some("@".into()),
        };
        match translate_key_down(&at_sign, false) {
            TranslatedKeyDown::Keys {
                raw_down,
                char_event: Some(ch),
            } => {
                assert_eq!(raw_down.windows_key_code, 0x32);
                assert_eq!(raw_down.native_key_code, 0x13);
                assert_ne!(raw_down.modifiers & EVENTFLAG_SHIFT_DOWN, 0);
                assert_eq!(ch.character, '@' as u16);
            }
            other => panic!("unexpected @ translation: {other:?}"),
        }
    }

    #[test]
    fn clipboard_shortcuts_and_emoji_use_editor_and_ime() {
        let paste = Keystroke {
            modifiers: Modifiers {
                platform: true,
                ..Default::default()
            },
            key: "v".into(),
            key_char: None,
        };
        assert!(matches!(
            translate_key_down(&paste, false),
            TranslatedKeyDown::Editor(EditOp::Paste)
        ));

        let redo = Keystroke {
            modifiers: Modifiers {
                platform: true,
                shift: true,
                ..Default::default()
            },
            key: "z".into(),
            key_char: None,
        };
        assert!(matches!(
            translate_key_down(&redo, false),
            TranslatedKeyDown::Editor(EditOp::Redo)
        ));

        let emoji = Keystroke {
            modifiers: Modifiers::default(),
            key: "🚀".into(),
            key_char: Some("🚀".into()),
        };
        assert!(matches!(
            translate_key_down(&emoji, false),
            TranslatedKeyDown::CommitText(ref s) if s == "🚀"
        ));
    }
}
