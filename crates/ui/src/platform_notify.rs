//! The macOS side of notifications: Dock badge, Dock bounce, alert sound,
//! and the banner. Everything here runs on the main thread (gpui's), and is
//! a no-op elsewhere.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// Whether Keel may post banners (`UNUserNotificationCenter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerPermission {
    /// Not the `.app` bundle (that API needs a bundle id), or not macOS.
    Unavailable,
    /// Not answered yet (or still being looked up).
    NotAsked,
    Allowed,
    /// Refused, or the permission prompt was dismissed: System Settings →
    /// Notifications → Keel turns it on.
    Denied,
}

const UNAVAILABLE: u8 = 0;
const NOT_ASKED: u8 = 1;
const ALLOWED: u8 = 2;
const DENIED: u8 = 3;
static PERMISSION: AtomicU8 = AtomicU8::new(UNAVAILABLE);
/// When the permission was last looked up (ms since the epoch).
static CHECKED_AT: AtomicU64 = AtomicU64::new(0);

pub fn banner_permission() -> BannerPermission {
    match PERMISSION.load(Ordering::Relaxed) {
        NOT_ASKED => BannerPermission::NotAsked,
        ALLOWED => BannerPermission::Allowed,
        DENIED => BannerPermission::Denied,
        _ => BannerPermission::Unavailable,
    }
}

/// System Settings → Notifications, on Keel's page.
pub const NOTIFICATION_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=com.keel.dev";

/// What clicked banners and `keel://` links ask of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiRequest {
    /// Bring Keel forward on this chat (empty: just bring it forward).
    OpenChat(String),
    /// The banner permission changed: redraw (Settings shows it).
    Refresh,
}

static REQUESTS: Mutex<Option<tokio::sync::mpsc::UnboundedSender<UiRequest>>> = Mutex::new(None);

/// Where clicked banners, `keel://` links and permission changes arrive.
/// One listener: a new call replaces the last.
pub fn ui_requests() -> tokio::sync::mpsc::UnboundedReceiver<UiRequest> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *REQUESTS.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    rx
}

fn send(request: UiRequest) {
    if let Some(tx) = REQUESTS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(request);
    }
}

pub fn request_open(chat_id: String) {
    send(UiRequest::OpenChat(chat_id));
}

fn set_permission(value: u8) {
    if PERMISSION.swap(value, Ordering::Relaxed) != value {
        send(UiRequest::Refresh);
    }
}

/// At launch, on the main thread: when Keel runs from its `.app` bundle,
/// take over banner clicks and look up the permission. With `ask` (banners
/// on) and nothing answered yet, macOS asks you.
pub fn init_native(ask: bool) {
    #[cfg(target_os = "macos")]
    native::init(ask);
    #[cfg(not(target_os = "macos"))]
    let _ = ask;
}

/// Ask macOS for permission (it prompts only while unanswered).
pub fn request_permission() {
    #[cfg(target_os = "macos")]
    if banner_permission() != BannerPermission::Unavailable {
        native::authorize();
    }
}

/// Look the permission up again (at most every 2 s): it can be turned on
/// or off in System Settings at any time.
pub fn refresh_permission() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    if banner_permission() == BannerPermission::Unavailable
        || now.saturating_sub(CHECKED_AT.load(Ordering::Relaxed)) < 2000
    {
        return;
    }
    CHECKED_AT.store(now, Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    native::check(false);
}

/// Opens Keel's page in System Settings → Notifications.
pub fn open_notification_settings() {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(NOTIFICATION_SETTINGS_URL)
        .spawn();
}

/// The Dock icon's badge; `None` clears it.
pub fn set_dock_badge(label: Option<&str>) {
    #[cfg(target_os = "macos")]
    mac::set_dock_badge(label);
    #[cfg(not(target_os = "macos"))]
    let _ = label;
}

/// One Dock bounce (macOS only bounces while Keel isn't frontmost).
pub fn bounce_dock() {
    #[cfg(target_os = "macos")]
    mac::bounce();
}

/// Plays `path` at `volume` (0–1). A new sound replaces one still playing.
pub fn play_sound(path: &Path, volume: f32) -> bool {
    #[cfg(target_os = "macos")]
    return mac::play(path, volume);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, volume);
        false
    }
}

/// Keel is the frontmost app.
pub fn app_active() -> bool {
    #[cfg(target_os = "macos")]
    return mac::app_active();
    #[cfg(not(target_os = "macos"))]
    true
}

/// A notification banner about `chat_id`, under Keel's name; clicking it
/// brings Keel forward on that chat. Posted only with permission: without
/// it there is no banner at all (sound and badge still go). There is no
/// AppleScript fallback any more: macOS files that under Script Editor, and
/// clicking it opened Script Editor instead of the chat.
/// Returns whether it was posted.
pub fn show_banner(title: &str, body: &str, chat_id: Option<&str>) -> bool {
    match banner_permission() {
        #[cfg(target_os = "macos")]
        BannerPermission::Allowed => {
            native::post(title, body, chat_id);
            true
        }
        _ => {
            // Maybe turned on in System Settings since: look again for next time.
            refresh_permission();
            let _ = (title, body, chat_id);
            false
        }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use std::sync::atomic::Ordering;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AnyThread as _, define_class, msg_send};
    use objc2_foundation::{NSBundle, NSDictionary, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };

    use std::ptr::NonNull;

    use objc2_user_notifications::{UNAuthorizationStatus, UNNotificationSettings};

    use super::{ALLOWED, DENIED, NOT_ASKED, PERMISSION, set_permission};
    use crate::notify::chat_from_payload;

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "KeelNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            /// Keel already skips banners for the chat you are looking at,
            /// so one that arrives while Keel is in front still shows.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                completion.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion: &block2::DynBlock<dyn Fn()>,
            ) {
                let info = response.notification().request().content().userInfo();
                let chat = chat_from_payload(|key| {
                    let key = NSString::from_str(key);
                    info.objectForKey(&key)
                        .and_then(|value| value.downcast::<NSString>().ok())
                        .map(|s| s.to_string())
                });
                tracing::info!(target: "keel_ui::notify", chat = ?chat, "banner clicked");
                // No chat id still brings Keel forward.
                super::request_open(chat.unwrap_or_default());
                completion.call(());
            }
        }
    );

    pub fn init(ask: bool) {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            return;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly: keep it for the app's life.
        std::mem::forget(delegate);
        PERMISSION.store(NOT_ASKED, Ordering::Relaxed);
        check(ask);
    }

    /// Looks the permission up; with `ask` and nothing answered yet, asks.
    pub fn check(ask: bool) {
        let done = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            let value = match status {
                UNAuthorizationStatus::Authorized
                | UNAuthorizationStatus::Provisional
                | UNAuthorizationStatus::Ephemeral => ALLOWED,
                UNAuthorizationStatus::Denied => DENIED,
                _ => NOT_ASKED,
            };
            if super::PERMISSION.load(Ordering::Relaxed) != value {
                tracing::info!(target: "keel_ui::notify", status = status.0, "banner permission");
            }
            set_permission(value);
            if value == NOT_ASKED && ask {
                authorize();
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&done);
    }

    /// Ask to post banners. macOS prompts once; later calls only report
    /// what you chose (System Settings → Notifications → Keel).
    pub fn authorize() {
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let done = RcBlock::new(|granted: Bool, error: *mut NSError| {
            let granted = granted.as_bool();
            let error = unsafe { error.as_ref() }
                .map(|e| format!("{} ({})", e.localizedDescription(), e.code()));
            tracing::info!(target: "keel_ui::notify", granted, ?error, "banner permission asked");
            // Dismissed or refused reads as Denied: settle it from the settings.
            check(false);
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Badge,
            &done,
        );
    }

    pub fn post(title: &str, body: &str, chat_id: Option<&str>) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        // No sound: Keel plays its own chime (Settings → Notifications).
        if let Some(chat_id) = chat_id {
            let [(key, value)] = crate::notify::banner_payload(chat_id);
            let (key, value) = (NSString::from_str(key), NSString::from_str(&value));
            let info: Retained<NSDictionary<NSString, NSString>> =
                NSDictionary::from_slices(&[&*key], &[&*value]);
            let info: Retained<NSDictionary<AnyObject, AnyObject>> =
                unsafe { Retained::cast_unchecked(info) };
            unsafe { content.setUserInfo(&info) };
        }
        let id = NSString::from_str(&format!("keel-{}", uuid::Uuid::new_v4()));
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
        let done = RcBlock::new(|error: *mut NSError| {
            if let Some(error) = unsafe { error.as_ref() } {
                tracing::warn!(target: "keel_ui::notify", error = %error.localizedDescription(), "banner not posted");
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&done));
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;
    use std::path::Path;

    use objc2::rc::Retained;
    use objc2::{AnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSRequestUserAttentionType, NSSound};
    use objc2_foundation::NSString;

    thread_local! {
        /// The sound playing now: NSSound stops if it is released mid-play.
        static PLAYING: RefCell<Option<Retained<NSSound>>> = const { RefCell::new(None) };
    }

    pub fn set_dock_badge(label: Option<&str>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let label = label.map(NSString::from_str);
        NSApplication::sharedApplication(mtm)
            .dockTile()
            .setBadgeLabel(label.as_deref());
    }

    pub fn bounce() {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm)
                .requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
        }
    }

    pub fn app_active() -> bool {
        MainThreadMarker::new().is_none_or(|mtm| NSApplication::sharedApplication(mtm).isActive())
    }

    pub fn play(path: &Path, volume: f32) -> bool {
        let Some(path) = path.to_str() else {
            return false;
        };
        let Some(sound) = NSSound::initWithContentsOfFile_byReference(
            NSSound::alloc(),
            &NSString::from_str(path),
            true,
        ) else {
            return false;
        };
        sound.setVolume(volume.clamp(0., 1.));
        PLAYING.with(|playing| {
            if let Some(old) = playing.borrow_mut().take() {
                old.stop();
            }
            let started = sound.play();
            *playing.borrow_mut() = Some(sound);
            started
        })
    }
}
