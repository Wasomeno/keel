//! Helper process executable for CEF subprocesses on macOS.
//!
//! CEF multi-process architecture requires dedicated helper bundles
//! (GPU, Renderer, Plugin, Alerts) that run this binary.

use cef::*;

wrap_app! {
    struct HelperApp {}
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
    }
}

fn main() {
    let args = cef::args::Args::new();
    #[cfg(target_os = "macos")]
    let _loader = {
        let exe = std::env::current_exe().expect("current exe");
        let loader = cef::library_loader::LibraryLoader::new(&exe, true);
        if !loader.load() {
            eprintln!("keel-helper: failed to load CEF framework");
        }
        loader
    };
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let mut app = HelperApp::new();
    let code = cef::execute_process(Some(&args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    std::process::exit(if code >= 0 { code } else { 0 });
}
