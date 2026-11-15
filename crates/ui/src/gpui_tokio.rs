//! Local replacement for the `gpui_tokio` crate that shipped with the zui
//! gpui fork. gpui-fast has no equivalent crate; Keel only used it to hand
//! the engine runtime to the viewport, so the three functions live here.

use gpui::{App, Global};
use tokio::runtime::{Handle, Runtime};

pub fn init(cx: &mut App) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("Failed to initialize Tokio");
    let handle = runtime.handle().clone();
    cx.set_global(GlobalTokio {
        owned_runtime: Some(runtime),
        handle,
    });
}

pub fn init_from_handle(cx: &mut App, handle: Handle) {
    cx.set_global(GlobalTokio {
        owned_runtime: None,
        handle,
    });
}

pub struct Tokio;

impl Tokio {
    pub fn handle(cx: &App) -> Handle {
        cx.global::<GlobalTokio>().handle.clone()
    }
}

struct GlobalTokio {
    owned_runtime: Option<Runtime>,
    handle: Handle,
}

impl Global for GlobalTokio {}

impl Drop for GlobalTokio {
    fn drop(&mut self) {
        if let Some(runtime) = self.owned_runtime.take() {
            runtime.shutdown_background();
        }
    }
}
