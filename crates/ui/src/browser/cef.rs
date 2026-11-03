//! The CEF-backed browser page: off-screen rendering (OSR) into a pixel buffer
//! drawn inside gpui. Real Chromium in its own process, agent tools driven over CDP.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use cef::*;
use gpui::{Bounds, Pixels, Point};

use super::agent::{ConsoleEntry, DialogInfo, DialogKind, site_allowed};
use super::{PageState, allowed_navigation, forwarded_key};

pub type Sender = tokio::sync::mpsc::UnboundedSender<NativeEvent>;

/// Size the page lays out at while it runs with the pane closed: a desktop
/// window, so pages and screenshots look like a normal browser's.
pub const BACKGROUND_SIZE: (f64, f64) = (1280., 800.);

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

static ACTIVE_WAKER_TX: Mutex<Option<Sender>> = Mutex::new(None);
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

            // The UI pump (`spin`) also drives paints. A delayed wake used to
            // spawn a thread per callback and re-enter the loop; the timer in
            // `schedule_work` is the one follow-up.
            if delay_ms <= 0
                && let Some(tx) = ACTIVE_WAKER_TX.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
            {
                let _ = tx.send(NativeEvent::Wake);
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
            eprintln!("[cef] on_before_browse: url={:?}", url);
            if !url.is_empty() && !allowed_navigation(&url) {
                eprintln!("[cef] on_before_browse BLOCKED: url={:?}", url);
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
    }
}

pub struct NativePage {
    browser: Browser,
    frame_state: Arc<Mutex<FrameState>>,
    shared_state: Arc<Mutex<SharedState>>,
    current_frame: RefCell<Option<Arc<gpui::RenderImage>>>,
    dropped_frame: RefCell<Option<Arc<gpui::RenderImage>>>,
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
    /// `set_windowless_frame_rate` last sent. Calling it every frame
    /// restarts the compositor clock and the picture jitters.
    frame_rate: Cell<i32>,
    background: Cell<bool>,
    shown: Cell<bool>,
    parked: Cell<bool>,
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

        *ACTIVE_WAKER_TX.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx.clone());

        Ok(Self {
            browser,
            frame_state,
            shared_state,
            current_frame: RefCell::new(None),
            dropped_frame: RefCell::new(None),
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
            frame_rate: Cell::new(60),
            background: Cell::new(false),
            shown: Cell::new(false),
            parked: Cell::new(false),
        })
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

    pub fn take_dropped_frame(&self) -> Option<Arc<gpui::RenderImage>> {
        self.dropped_frame.borrow_mut().take()
    }

    pub fn current_frame(&self) -> Option<Arc<gpui::RenderImage>> {
        self.current_frame.borrow().clone()
    }

    pub fn update_frame(&self) -> bool {
        let mut fs = self.frame_state.lock().unwrap();
        if fs.has_new && !fs.buffer.is_empty() {
            fs.has_new = false;
            let w = fs.buf_w;
            let h = fs.buf_h;
            if let Some(rgba) = image::RgbaImage::from_raw(w, h, fs.buffer.clone()) {
                let render_img = Arc::new(gpui::RenderImage::new(vec![image::Frame::new(rgba)]));
                if let Some(old) = self.current_frame.borrow_mut().replace(render_img) {
                    *self.dropped_frame.borrow_mut() = Some(old);
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

    pub fn handle_mouse_down(&self, pos: Point<Pixels>, button: gpui::MouseButton) {
        let origin = self.origin.get();
        let rel_x = (f32::from(pos.x - origin.x)).max(0.0);
        let rel_y = (f32::from(pos.y - origin.y)).max(0.0);
        let event = MouseEvent {
            x: rel_x.round() as i32,
            y: rel_y.round() as i32,
            modifiers: 0,
        };
        let btn = match button {
            gpui::MouseButton::Left => MouseButtonType::LEFT,
            gpui::MouseButton::Middle => MouseButtonType::MIDDLE,
            gpui::MouseButton::Right => MouseButtonType::RIGHT,
            gpui::MouseButton::Navigate(_) => return,
        };
        if let Some(host) = self.browser.host() {
            host.send_mouse_click_event(Some(&event), btn, 0, 1);
        }
    }

    pub fn handle_mouse_up(&self, pos: Point<Pixels>, button: gpui::MouseButton) {
        let origin = self.origin.get();
        let rel_x = (f32::from(pos.x - origin.x)).max(0.0);
        let rel_y = (f32::from(pos.y - origin.y)).max(0.0);
        let event = MouseEvent {
            x: rel_x.round() as i32,
            y: rel_y.round() as i32,
            modifiers: 0,
        };
        let btn = match button {
            gpui::MouseButton::Left => MouseButtonType::LEFT,
            gpui::MouseButton::Middle => MouseButtonType::MIDDLE,
            gpui::MouseButton::Right => MouseButtonType::RIGHT,
            gpui::MouseButton::Navigate(_) => return,
        };
        if let Some(host) = self.browser.host() {
            host.send_mouse_click_event(Some(&event), btn, 1, 1);
        }
    }

    pub fn handle_mouse_move(&self, pos: Point<Pixels>) {
        let origin = self.origin.get();
        let rel_x = (f32::from(pos.x - origin.x)).max(0.0);
        let rel_y = (f32::from(pos.y - origin.y)).max(0.0);
        let event = MouseEvent {
            x: rel_x.round() as i32,
            y: rel_y.round() as i32,
            modifiers: 0,
        };
        if let Some(host) = self.browser.host() {
            host.send_mouse_move_event(Some(&event), 0);
        }
    }

    pub fn handle_scroll_wheel(&self, pos: Point<Pixels>, delta: gpui::ScrollDelta) {
        let origin = self.origin.get();
        let rel_x = (f32::from(pos.x - origin.x)).max(0.0);
        let rel_y = (f32::from(pos.y - origin.y)).max(0.0);
        let event = MouseEvent {
            x: rel_x.round() as i32,
            y: rel_y.round() as i32,
            modifiers: 0,
        };
        let (dx, dy) = match delta {
            gpui::ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            gpui::ScrollDelta::Lines(p) => (p.x * 40.0, p.y * 40.0),
        };
        if let Some(host) = self.browser.host() {
            host.send_mouse_wheel_event(Some(&event), dx.round() as i32, dy.round() as i32);
        }
    }

    pub fn handle_key_down(&self, event: &gpui::KeyDownEvent) -> bool {
        let key = &event.keystroke.key;
        if forwarded_key(
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
            key,
        ) {
            return false;
        }
        let vk = key_to_vk(key);
        let ch = if key.chars().count() == 1 {
            key.chars().next().unwrap()
        } else {
            '\0'
        };
        let mut modifiers = 0u32;
        if event.keystroke.modifiers.shift {
            modifiers |= 1 << 0;
        }
        if event.keystroke.modifiers.control {
            modifiers |= 1 << 1;
        }
        if event.keystroke.modifiers.alt {
            modifiers |= 1 << 2;
        }
        if event.keystroke.modifiers.platform {
            modifiers |= 1 << 3;
        }

        let key_event = KeyEvent {
            size: std::mem::size_of::<sys::_cef_key_event_t>(),
            type_: KeyEventType::RAWKEYDOWN,
            modifiers,
            windows_key_code: vk,
            native_key_code: 0,
            is_system_key: 0,
            character: ch as u16,
            unmodified_character: ch as u16,
            focus_on_editable_field: 0,
        };
        let char_event = if ch != '\0'
            && !event.keystroke.modifiers.platform
            && !event.keystroke.modifiers.control
        {
            Some(KeyEvent {
                type_: KeyEventType::CHAR,
                ..key_event
            })
        } else {
            None
        };
        // Not now: `send_key_event` walks the menu bar, and validating a
        // menu item borrows the UI again. That abort is the crash.
        self.defer_key(key_event, char_event);
        true
    }

    pub fn handle_key_up(&self, event: &gpui::KeyUpEvent) -> bool {
        let key = &event.keystroke.key;
        if forwarded_key(
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
            key,
        ) {
            return false;
        }
        let vk = key_to_vk(key);
        let ch = if key.chars().count() == 1 {
            key.chars().next().unwrap()
        } else {
            '\0'
        };
        let mut modifiers = 0u32;
        if event.keystroke.modifiers.shift {
            modifiers |= 1 << 0;
        }
        if event.keystroke.modifiers.control {
            modifiers |= 1 << 1;
        }
        if event.keystroke.modifiers.alt {
            modifiers |= 1 << 2;
        }
        if event.keystroke.modifiers.platform {
            modifiers |= 1 << 3;
        }

        let key_event = KeyEvent {
            size: std::mem::size_of::<sys::_cef_key_event_t>(),
            type_: KeyEventType::KEYUP,
            modifiers,
            windows_key_code: vk,
            native_key_code: 0,
            is_system_key: 0,
            character: ch as u16,
            unmodified_character: ch as u16,
            focus_on_editable_field: 0,
        };
        self.defer_key(key_event, None);
        true
    }

    pub fn focus_parent(&self) {
        if let Some(host) = self.browser.host() {
            host.set_focus(1);
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
    fn defer_key(&self, event: KeyEvent, char_event: Option<KeyEvent>) {
        #[cfg(target_os = "macos")]
        {
            struct Job {
                browser: Browser,
                event: KeyEvent,
                char_event: Option<KeyEvent>,
            }
            extern "C" fn fire(ctx: *mut std::ffi::c_void) {
                let job = unsafe { Box::from_raw(ctx as *mut Job) };
                if let Some(host) = job.browser.host() {
                    host.send_key_event(Some(&job.event));
                    if let Some(ch) = job.char_event.as_ref() {
                        host.send_key_event(Some(ch));
                    }
                }
            }
            let job = Box::new(Job {
                browser: self.browser.clone(),
                event,
                char_event,
            });
            mac_dispatch::after(Box::into_raw(job) as *mut std::ffi::c_void, fire);
        }
        #[cfg(not(target_os = "macos"))]
        {
            if let Some(host) = self.browser.host() {
                host.send_key_event(Some(&event));
                if let Some(ch) = char_event.as_ref() {
                    host.send_key_event(Some(ch));
                }
            }
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
        self.spin();
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
        if let Some(host) = self.browser.host() {
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

fn key_to_vk(key: &str) -> i32 {
    match key {
        "enter" => 0x0D,
        "escape" => 0x1B,
        "tab" => 0x09,
        "backspace" => 0x08,
        "space" => 0x20,
        "arrowup" | "up" => 0x26,
        "arrowdown" | "down" => 0x28,
        "arrowleft" | "left" => 0x25,
        "arrowright" | "right" => 0x27,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "home" => 0x24,
        "end" => 0x23,
        "delete" => 0x2E,
        k if k.len() == 1 => {
            let c = k.chars().next().unwrap().to_ascii_uppercase();
            c as i32
        }
        _ => 0,
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
