//! macOS-specific window vibrancy, blur, color space, Dock and app lifecycle
//! integration. Talks to the Objective-C runtime directly (no third-party crates).
//! Every function has a no-op twin on other platforms so callers need no `cfg`.

#[cfg(target_os = "macos")]
pub use imp::*;

#[cfg(not(target_os = "macos"))]
pub use stub::*;

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_void, CStr};
    use std::sync::atomic::{AtomicBool, Ordering};

    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    use crate::config::{ColorSpace, Config};

    type Id = *mut c_void;

    #[link(name = "objc", kind = "dylib")]
    extern "C" {
        fn objc_getClass(name: *const std::ffi::c_char) -> Id;
        fn sel_registerName(name: *const std::ffi::c_char) -> Id;
        fn objc_msgSend();
        fn class_addMethod(cls: Id, name: Id, imp: *const (), types: *const std::ffi::c_char) -> bool;
        fn class_getInstanceMethod(cls: Id, name: Id) -> Id;
        fn method_setImplementation(m: Id, imp: *const ()) -> *const ();
    }

    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
    }

    fn sel(name: &CStr) -> Id {
        // SAFETY: registering a selector name is always valid.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    fn class(name: &CStr) -> Option<Id> {
        // SAFETY: looking up a class by name is always valid.
        let cls = unsafe { objc_getClass(name.as_ptr()) };
        (!cls.is_null()).then_some(cls)
    }

    /// Send an Objective-C message with an explicit C signature:
    /// `msg_send!(receiver, c"selector:", arg: Type, ... => ReturnType)`.
    /// Must be used inside `unsafe`: the signature has to match the method.
    macro_rules! msg_send {
        ($recv:expr, $sel:expr $(, $arg:expr => $ty:ty)* ; -> $ret:ty) => {{
            let f: unsafe extern "C" fn(Id, Id $(, $ty)*) -> $ret =
                std::mem::transmute(objc_msgSend as *const ());
            f($recv, sel($sel) $(, $arg)*)
        }};
        ($recv:expr, $sel:expr $(, $arg:expr => $ty:ty)*) => {
            msg_send!($recv, $sel $(, $arg => $ty)* ; -> ())
        };
    }

    /// Send a message that returns an object; `None` for nil.
    unsafe fn send_id(receiver: Id, selector: &CStr) -> Option<Id> {
        let obj = msg_send!(receiver, selector; -> Id);
        (!obj.is_null()).then_some(obj)
    }

    unsafe fn responds_to(obj: Id, selector: &CStr) -> bool {
        msg_send!(obj, c"respondsToSelector:", sel(selector) => Id; -> bool)
    }

    unsafe fn ns_string(s: &str) -> Option<Id> {
        let c = std::ffi::CString::new(s).ok()?;
        let obj = msg_send!(class(c"NSString")?, c"stringWithUTF8String:", c.as_ptr() => *const std::ffi::c_char; -> Id);
        (!obj.is_null()).then_some(obj)
    }

    unsafe fn shared_application() -> Option<Id> {
        send_id(class(c"NSApplication")?, c"sharedApplication")
    }

    /// Call `f` for each element of an `NSArray`.
    unsafe fn for_each_in_array(array: Id, mut f: impl FnMut(Id)) {
        let count = msg_send!(array, c"count"; -> usize);
        for i in 0..count {
            let obj = msg_send!(array, c"objectAtIndex:", i => usize; -> Id);
            if !obj.is_null() {
                f(obj);
            }
        }
    }

    fn ns_window_of(window: &Window) -> Option<(Id, Id)> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return None };
        let ns_view = appkit.ns_view.as_ptr() as Id;
        // SAFETY: `ns_view` is the live NSView backing this winit window.
        let ns_window = unsafe { send_id(ns_view, c"window")? };
        Some((ns_view, ns_window))
    }

    /// Return freed heap pages to the OS (keeps idle RSS low after closing tabs).
    pub fn release_free_memory() {
        // SAFETY: a null zone means "all zones"; goal 0 releases as much as possible.
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
    }

    pub fn apply_macos_window_config(window: &Window, cfg: &Config) {
        let Some((ns_view, ns_window)) = ns_window_of(window) else { return };
        // SAFETY: every message below is sent to live AppKit objects with the
        // signature documented by Apple for that selector.
        unsafe {
            apply_color_space(ns_view, ns_window, cfg.window_colorspace);

            // Seamless titlebar (Ghostty-style): transparent, no separator line.
            msg_send!(ns_window, c"setTitlebarAppearsTransparent:", true => bool);
            if responds_to(ns_window, c"setTitlebarSeparatorStyle:") {
                // NSTitlebarSeparatorStyleNone = 1
                msg_send!(ns_window, c"setTitlebarSeparatorStyle:", 1 => i64);
            }

            let channel = |shift: u32| ((cfg.background >> shift) & 0xFF) as f64 / 255.0;
            let (r, g, b) = (channel(24), channel(16), channel(8));
            let a = cfg.background_opacity as f64;

            // Dark/light appearance so title text and window controls contrast cleanly.
            let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
            let name = if luminance > 0.5 { "NSAppearanceNameAqua" } else { "NSAppearanceNameDarkAqua" };
            if let (Some(cls), Some(name)) = (class(c"NSAppearance"), ns_string(name)) {
                let appearance = msg_send!(cls, c"appearanceNamed:", name => Id; -> Id);
                if !appearance.is_null() {
                    msg_send!(ns_window, c"setAppearance:", appearance => Id);
                }
            }

            // Window background exactly matching the terminal background.
            if let Some(cls) = class(c"NSColor") {
                let color = msg_send!(
                    cls, c"colorWithSRGBRed:green:blue:alpha:", r => f64, g => f64, b => f64, a => f64; -> Id
                );
                if !color.is_null() {
                    msg_send!(ns_window, c"setBackgroundColor:", color => Id);
                }
            }

            msg_send!(ns_window, c"setOpaque:", cfg.background_opacity >= 1.0 => bool);

            if cfg.background_blur > 0 && cfg.background_opacity < 1.0 {
                setup_visual_effect_view(ns_window);
            }
        }
    }

    /// Set the window and CAMetalLayer color space so Metal outputs sRGB or Display P3.
    unsafe fn apply_color_space(ns_view: Id, ns_window: Id, space: ColorSpace) {
        let Some(cls) = class(c"NSColorSpace") else { return };
        let ctor = match space {
            ColorSpace::DisplayP3 => c"displayP3ColorSpace",
            ColorSpace::Srgb => c"sRGBColorSpace",
        };
        let Some(cs) = send_id(cls, ctor) else { return };
        msg_send!(ns_window, c"setColorSpace:", cs => Id);

        let Some(cg_cs) = send_id(cs, c"CGColorSpace") else { return };
        if let Some(layer) = send_id(ns_view, c"layer") {
            apply_layer_colorspace(layer, cg_cs);
        }
        if let Some(content_view) = send_id(ns_window, c"contentView").filter(|&v| v != ns_view) {
            if let Some(layer) = send_id(content_view, c"layer") {
                apply_layer_colorspace(layer, cg_cs);
            }
        }
        if let Some(subviews) = send_id(ns_view, c"subviews") {
            for_each_in_array(subviews, |sv| {
                if let Some(layer) = send_id(sv, c"layer") {
                    apply_layer_colorspace(layer, cg_cs);
                }
            });
        }
    }

    unsafe fn apply_layer_colorspace(layer: Id, cg_cs: Id) {
        for setter in [c"setColorspace:", c"setColorSpace:"] {
            if responds_to(layer, setter) {
                msg_send!(layer, setter, cg_cs => Id);
            }
        }
        if let Some(sublayers) = send_id(layer, c"sublayers") {
            for_each_in_array(sublayers, |sub| apply_layer_colorspace(sub, cg_cs));
        }
    }

    static BLUR_SETUP: AtomicBool = AtomicBool::new(false);

    /// Background blur: an NSVisualEffectView behind the window content.
    unsafe fn setup_visual_effect_view(ns_window: Id) {
        if BLUR_SETUP.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(content_view) = send_id(ns_window, c"contentView") else { return };
        let Some(alloc) = class(c"NSVisualEffectView").and_then(|cls| send_id(cls, c"alloc")) else { return };

        #[repr(C)]
        #[derive(Copy, Clone)]
        struct NSRect {
            x: f64,
            y: f64,
            w: f64,
            h: f64,
        }
        let bounds = msg_send!(content_view, c"bounds"; -> NSRect);
        let effect_view = msg_send!(alloc, c"initWithFrame:", bounds => NSRect; -> Id);
        if effect_view.is_null() {
            return;
        }
        // NSViewWidthSizable (2) | NSViewHeightSizable (16)
        msg_send!(effect_view, c"setAutoresizingMask:", 18 => u64);
        // NSVisualEffectMaterialUnderWindowBackground
        msg_send!(effect_view, c"setMaterial:", 11 => i64);
        // NSVisualEffectBlendingModeBehindWindow
        msg_send!(effect_view, c"setBlendingMode:", 0 => i64);
        // NSVisualEffectStateActive
        msg_send!(effect_view, c"setState:", 1 => i64);
        // [contentView addSubview:effect_view positioned:NSWindowBelow relativeTo:nil]
        msg_send!(
            content_view, c"addSubview:positioned:relativeTo:",
            effect_view => Id, -1 => i64, std::ptr::null_mut() => Id
        );
    }

    static REOPEN_PROXY: parking_lot::Mutex<Option<winit::event_loop::EventLoopProxy<crate::AppEvent>>> =
        parking_lot::Mutex::new(None);

    fn send_reopen() {
        if let Some(proxy) = REOPEN_PROXY.lock().as_ref() {
            let _ = proxy.send_event(crate::AppEvent::Reopen);
        }
    }

    /// Reopen a window when the Dock icon is clicked or the app is activated.
    pub fn register_reopen_handler(proxy: winit::event_loop::EventLoopProxy<crate::AppEvent>) {
        *REOPEN_PROXY.lock() = Some(proxy);

        extern "C" fn on_reopen(_this: Id, _cmd: Id, _app: Id, _has_visible_windows: bool) -> bool {
            send_reopen();
            true
        }
        extern "C" fn on_activate(_this: Id, _cmd: Id, _notification: Id) {
            send_reopen();
        }

        // SAFETY: the method implementations match the type encodings given.
        unsafe {
            let Some(delegate) = shared_application().and_then(|app| send_id(app, c"delegate")) else { return };
            let Some(delegate_cls) = send_id(delegate, c"class") else { return };
            let methods: [(&CStr, *const (), &CStr); 2] = [
                (c"applicationShouldHandleReopen:hasVisibleWindows:", on_reopen as *const (), c"B@:@B"),
                (c"applicationDidBecomeActive:", on_activate as *const (), c"v@:@"),
            ];
            for (name, imp, types) in methods {
                let selector = sel(name);
                if !class_addMethod(delegate_cls, selector, imp, types.as_ptr()) {
                    let m = class_getInstanceMethod(delegate_cls, selector);
                    if !m.is_null() {
                        method_setImplementation(m, imp);
                    }
                }
            }
        }
    }

    pub fn set_dock_icon() {
        // SAFETY: standard NSApplication / NSImage calls on live objects.
        unsafe {
            let Some(app) = shared_application() else { return };

            // Inside a .app bundle the Dock already reads Laplace.icns from the bundle;
            // skipping setApplicationIconImage saves ~8 MB of resident bitmap memory.
            let bundle_path = class(c"NSBundle")
                .and_then(|cls| send_id(cls, c"mainBundle"))
                .and_then(|bundle| send_id(bundle, c"bundlePath"))
                .map(|path| msg_send!(path, c"UTF8String"; -> *const std::ffi::c_char))
                .filter(|p| !p.is_null());
            if let Some(path) = bundle_path {
                if CStr::from_ptr(path).to_string_lossy().ends_with(".app") {
                    return;
                }
            }

            const ICON_BYTES: &[u8] = include_bytes!("../resources/icon.png");
            let Some(data_cls) = class(c"NSData") else { return };
            let ns_data = msg_send!(
                data_cls, c"dataWithBytes:length:", ICON_BYTES.as_ptr() => *const u8, ICON_BYTES.len() => usize; -> Id
            );
            if ns_data.is_null() {
                return;
            }
            let Some(alloc) = class(c"NSImage").and_then(|cls| send_id(cls, c"alloc")) else { return };
            let image = msg_send!(alloc, c"initWithData:", ns_data => Id; -> Id);
            if image.is_null() {
                return;
            }
            msg_send!(app, c"setApplicationIconImage:", image => Id);
            msg_send!(image, c"release");
        }
    }

    pub fn activate_app() {
        // SAFETY: standard NSApplication call.
        unsafe {
            if let Some(app) = shared_application() {
                msg_send!(app, c"activateIgnoringOtherApps:", true => bool);
            }
        }
    }

    /// Show the shell's working directory as the window's proxy icon.
    pub fn set_window_represented_filename(window: &Window, path: &str) {
        let Some((_, ns_window)) = ns_window_of(window) else { return };
        // SAFETY: NSWindow.setRepresentedFilename: takes an NSString.
        unsafe {
            if let Some(path) = ns_string(path) {
                msg_send!(ns_window, c"setRepresentedFilename:", path => Id);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
mod stub {
    pub fn release_free_memory() {}
    pub fn apply_macos_window_config(_window: &winit::window::Window, _cfg: &crate::config::Config) {}
    pub fn register_reopen_handler(_proxy: winit::event_loop::EventLoopProxy<crate::AppEvent>) {}
    pub fn set_dock_icon() {}
    pub fn activate_app() {}
    pub fn set_window_represented_filename(_window: &winit::window::Window, _path: &str) {}
}
