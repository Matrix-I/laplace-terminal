//! macOS-specific window vibrancy, blur, and color space configuration.
//! Uses the Objective-C runtime via `libc` without third-party dependencies.

#[cfg(target_os = "macos")]
use std::ffi::c_void;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "macos")]
use winit::raw_window_handle::HasWindowHandle;
#[cfg(target_os = "macos")]
use winit::window::Window;

#[cfg(target_os = "macos")]
use crate::config::{ColorSpace, Config};

#[cfg(target_os = "macos")]
pub fn apply_macos_window_config(window: &Window, cfg: &Config) {
    let Ok(handle) = window.window_handle() else { return };
    let winit::raw_window_handle::RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr() as *mut c_void;
    if ns_view.is_null() {
        return;
    }

    unsafe {
        let ns_window = msg_send_ptr(ns_view, "window\0");
        if ns_window.is_null() {
            return;
        }

        // 1. Color space
        let nscolor_space_cls = objc_getClass(b"NSColorSpace\0".as_ptr());
        if !nscolor_space_cls.is_null() {
            let sel = match cfg.window_colorspace {
                ColorSpace::DisplayP3 => "displayP3ColorSpace\0",
                ColorSpace::Srgb => "sRGBColorSpace\0",
            };
            let cs = msg_send_ptr(nscolor_space_cls, sel);
            if !cs.is_null() {
                let set_cs_sel = sel_registerName(b"setColorSpace:\0".as_ptr());
                let f: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                    std::mem::transmute(objc_msgSend as *const ());
                f(ns_window, set_cs_sel, cs);

                // Set CAMetalLayer colorspace so Metal accurately outputs in sRGB or Display P3
                let cg_cs = msg_send_ptr(cs, "CGColorSpace\0");
                if !cg_cs.is_null() {
                    let layer = msg_send_ptr(ns_view, "layer\0");
                    if !layer.is_null() {
                        apply_layer_colorspace(layer, cg_cs);
                    }
                    let content_view = msg_send_ptr(ns_window, "contentView\0");
                    if !content_view.is_null() && content_view != ns_view {
                        let cv_layer = msg_send_ptr(content_view, "layer\0");
                        if !cv_layer.is_null() {
                            apply_layer_colorspace(cv_layer, cg_cs);
                        }
                    }
                    let subviews = msg_send_ptr(ns_view, "subviews\0");
                    if !subviews.is_null() {
                        let count_sel = sel_registerName(b"count\0".as_ptr());
                        let count_fn: extern "C" fn(*mut c_void, *mut c_void) -> usize =
                            std::mem::transmute(objc_msgSend as *const ());
                        let count = count_fn(subviews, count_sel);
                        let obj_at_sel = sel_registerName(b"objectAtIndex:\0".as_ptr());
                        let obj_fn: extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void =
                            std::mem::transmute(objc_msgSend as *const ());
                        for i in 0..count {
                            let sv = obj_fn(subviews, obj_at_sel, i);
                            if !sv.is_null() {
                                let sub_layer = msg_send_ptr(sv, "layer\0");
                                if !sub_layer.is_null() {
                                    apply_layer_colorspace(sub_layer, cg_cs);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Seamless titlebar (Ghostty-style: transparent titlebar, no separator line, matching background)
        let set_transparent_sel = sel_registerName(b"setTitlebarAppearsTransparent:\0".as_ptr());
        let f_trans: extern "C" fn(*mut c_void, *mut c_void, bool) =
            std::mem::transmute(objc_msgSend as *const ());
        f_trans(ns_window, set_transparent_sel, true);

        // Remove titlebar separator line (NSTitlebarSeparatorStyleNone = 1)
        let sep_sel = sel_registerName(b"setTitlebarSeparatorStyle:\0".as_ptr());
        let responds_sel = sel_registerName(b"respondsToSelector:\0".as_ptr());
        let responds_fn: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool =
            std::mem::transmute(objc_msgSend as *const ());
        if responds_fn(ns_window, responds_sel, sep_sel) {
            let f_sep: extern "C" fn(*mut c_void, *mut c_void, i64) =
                std::mem::transmute(objc_msgSend as *const ());
            f_sep(ns_window, sep_sel, 1);
        }

        // Set dark/light appearance so title text and window controls contrast cleanly
        let r = ((cfg.background >> 24) & 0xFF) as f64 / 255.0;
        let g = ((cfg.background >> 16) & 0xFF) as f64 / 255.0;
        let b = ((cfg.background >> 8) & 0xFF) as f64 / 255.0;
        let a = cfg.background_opacity as f64;
        let luminance = 0.299 * r + 0.587 * g + 0.114 * b;

        let appearance_cls = objc_getClass(b"NSAppearance\0".as_ptr());
        if !appearance_cls.is_null() {
            let app_name = if luminance > 0.5 {
                "NSAppearanceNameAqua"
            } else {
                "NSAppearanceNameDarkAqua"
            };
            let app_str = ns_string(app_name);
            if !app_str.is_null() {
                let app_named_sel = sel_registerName(b"appearanceNamed:\0".as_ptr());
                let app_named_fn: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
                    std::mem::transmute(objc_msgSend as *const ());
                let appearance = app_named_fn(appearance_cls, app_named_sel, app_str);
                if !appearance.is_null() {
                    let set_app_sel = sel_registerName(b"setAppearance:\0".as_ptr());
                    let set_app_fn: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                        std::mem::transmute(objc_msgSend as *const ());
                    set_app_fn(ns_window, set_app_sel, appearance);
                }
            }
        }

        // Set window background color exactly matching terminal background
        let nscolor_cls = objc_getClass(b"NSColor\0".as_ptr());
        if !nscolor_cls.is_null() {
            let color_sel = sel_registerName(b"colorWithSRGBRed:green:blue:alpha:\0".as_ptr());
            let color_fn: extern "C" fn(*mut c_void, *mut c_void, f64, f64, f64, f64) -> *mut c_void =
                std::mem::transmute(objc_msgSend as *const ());
            let bg_color = color_fn(nscolor_cls, color_sel, r, g, b, a);
            if !bg_color.is_null() {
                let set_bg_sel = sel_registerName(b"setBackgroundColor:\0".as_ptr());
                let set_bg_fn: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                    std::mem::transmute(objc_msgSend as *const ());
                set_bg_fn(ns_window, set_bg_sel, bg_color);
            }
        }

        let is_opaque = cfg.background_opacity >= 1.0;
        let set_opaque_sel = sel_registerName(b"setOpaque:\0".as_ptr());
        let f_op: extern "C" fn(*mut c_void, *mut c_void, bool) =
            std::mem::transmute(objc_msgSend as *const ());
        f_op(ns_window, set_opaque_sel, is_opaque);

        // 3. Background blur (NSVisualEffectView behind window content)
        if cfg.background_blur > 0 && cfg.background_opacity < 1.0 {
            setup_visual_effect_view(ns_window);
        }
    }
}

#[cfg(target_os = "macos")]
static BLUR_SETUP: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
unsafe fn setup_visual_effect_view(ns_window: *mut c_void) {
    if BLUR_SETUP.swap(true, Ordering::SeqCst) {
        return;
    }
    let content_view = msg_send_ptr(ns_window, "contentView\0");
    if content_view.is_null() {
        return;
    }
    let ve_cls = objc_getClass(b"NSVisualEffectView\0".as_ptr());
    if ve_cls.is_null() {
        return;
    }
    let alloc = msg_send_ptr(ve_cls, "alloc\0");
    if alloc.is_null() {
        return;
    }

    // [contentView bounds]
    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct NSRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    let bounds_sel = sel_registerName(b"bounds\0".as_ptr());
    let get_bounds: extern "C" fn(*mut c_void, *mut c_void) -> NSRect =
        std::mem::transmute(objc_msgSend as *const ());
    let bounds: NSRect = get_bounds(content_view, bounds_sel);

    // [[alloc] initWithFrame:bounds]
    let init_sel = sel_registerName(b"initWithFrame:\0".as_ptr());
    let init_frame: extern "C" fn(*mut c_void, *mut c_void, NSRect) -> *mut c_void =
        std::mem::transmute(objc_msgSend as *const ());
    let effect_view = init_frame(alloc, init_sel, bounds);
    if effect_view.is_null() {
        return;
    }

    // autoresizingMask = NSViewWidthSizable (2) | NSViewHeightSizable (16) = 18
    let set_autoresize = sel_registerName(b"setAutoresizingMask:\0".as_ptr());
    let set_u64: extern "C" fn(*mut c_void, *mut c_void, u64) =
        std::mem::transmute(objc_msgSend as *const ());
    set_u64(effect_view, set_autoresize, 18);

    // material = NSVisualEffectMaterialUnderWindowBackground (11) or Dark (2)
    let set_mat = sel_registerName(b"setMaterial:\0".as_ptr());
    let set_i64: extern "C" fn(*mut c_void, *mut c_void, i64) =
        std::mem::transmute(objc_msgSend as *const ());
    set_i64(effect_view, set_mat, 11);

    // blendingMode = NSVisualEffectBlendingModeBehindWindow (0)
    let set_blend = sel_registerName(b"setBlendingMode:\0".as_ptr());
    set_i64(effect_view, set_blend, 0);

    // state = NSVisualEffectStateActive (1)
    let set_state = sel_registerName(b"setState:\0".as_ptr());
    set_i64(effect_view, set_state, 1);

    // [contentView addSubview:effect_view positioned:NSWindowBelow relativeTo:nil]
    let add_subview =
        sel_registerName(b"addSubview:positioned:relativeTo:\0".as_ptr());
    let add_fn: extern "C" fn(*mut c_void, *mut c_void, *mut c_void, i64, *mut c_void) =
        std::mem::transmute(objc_msgSend as *const ());
    add_fn(content_view, add_subview, effect_view, -1, std::ptr::null_mut());
}

#[cfg(target_os = "macos")]
unsafe fn apply_layer_colorspace(layer: *mut c_void, cg_cs: *mut c_void) {
    if layer.is_null() {
        return;
    }
    let set_cs1 = sel_registerName(b"setColorspace:\0".as_ptr());
    let set_cs2 = sel_registerName(b"setColorSpace:\0".as_ptr());
    let responds_sel = sel_registerName(b"respondsToSelector:\0".as_ptr());
    let responds_fn: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool =
        std::mem::transmute(objc_msgSend as *const ());
    if responds_fn(layer, responds_sel, set_cs1) {
        let f: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        f(layer, set_cs1, cg_cs);
    }
    if responds_fn(layer, responds_sel, set_cs2) {
        let f: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        f(layer, set_cs2, cg_cs);
    }
    let sublayers = msg_send_ptr(layer, "sublayers\0");
    if !sublayers.is_null() {
        let count_sel = sel_registerName(b"count\0".as_ptr());
        let count_fn: extern "C" fn(*mut c_void, *mut c_void) -> usize =
            std::mem::transmute(objc_msgSend as *const ());
        let count = count_fn(sublayers, count_sel);
        let obj_at_sel = sel_registerName(b"objectAtIndex:\0".as_ptr());
        let obj_fn: extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        for i in 0..count {
            let sub = obj_fn(sublayers, obj_at_sel, i);
            if !sub.is_null() {
                apply_layer_colorspace(sub, cg_cs);
            }
        }
    }
}

#[cfg(target_os = "macos")]
unsafe fn msg_send_ptr(receiver: *mut c_void, sel_name: &str) -> *mut c_void {
    let mut null_terminated = Vec::with_capacity(sel_name.len() + 1);
    null_terminated.extend_from_slice(sel_name.as_bytes());
    if !sel_name.ends_with('\0') {
        null_terminated.push(0);
    }
    let sel = sel_registerName(null_terminated.as_ptr());
    let f: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
        std::mem::transmute(objc_msgSend as *const ());
    f(receiver, sel)
}

#[cfg(target_os = "macos")]
static REOPEN_PROXY: parking_lot::Mutex<Option<winit::event_loop::EventLoopProxy<crate::AppEvent>>> =
    parking_lot::Mutex::new(None);

#[cfg(target_os = "macos")]
pub fn register_reopen_handler(proxy: winit::event_loop::EventLoopProxy<crate::AppEvent>) {
    *REOPEN_PROXY.lock() = Some(proxy);

    unsafe {
        let app_cls = objc_getClass(b"NSApplication\0".as_ptr());
        if app_cls.is_null() {
            return;
        }
        let app = msg_send_ptr(app_cls, "sharedApplication\0");
        if app.is_null() {
            return;
        }
        let delegate = msg_send_ptr(app, "delegate\0");
        if delegate.is_null() {
            return;
        }
        let delegate_cls: *mut c_void = msg_send_ptr(delegate, "class\0");
        if delegate_cls.is_null() {
            return;
        }

        extern "C" fn on_reopen(
            _this: *mut c_void,
            _cmd: *mut c_void,
            _app: *mut c_void,
            _has_visible_windows: bool,
        ) -> bool {
            if let Some(proxy) = REOPEN_PROXY.lock().as_ref() {
                let _ = proxy.send_event(crate::AppEvent::Reopen);
            }
            true
        }

        extern "C" fn on_activate(_this: *mut c_void, _cmd: *mut c_void, _notif: *mut c_void) {
            if let Some(proxy) = REOPEN_PROXY.lock().as_ref() {
                let _ = proxy.send_event(crate::AppEvent::Reopen);
            }
        }

        extern "C" {
            fn class_getInstanceMethod(cls: *mut c_void, name: *mut c_void) -> *mut c_void;
            fn method_setImplementation(m: *mut c_void, imp: *const ()) -> *const ();
        }

        let sel_reopen = sel_registerName(b"applicationShouldHandleReopen:hasVisibleWindows:\0".as_ptr());
        if !class_addMethod(delegate_cls, sel_reopen, on_reopen as *const (), b"B@:@B\0".as_ptr()) {
            let m = class_getInstanceMethod(delegate_cls, sel_reopen);
            if !m.is_null() {
                method_setImplementation(m, on_reopen as *const ());
            }
        }

        let sel_act = sel_registerName(b"applicationDidBecomeActive:\0".as_ptr());
        if !class_addMethod(delegate_cls, sel_act, on_activate as *const (), b"v@:@\0".as_ptr()) {
            let m = class_getInstanceMethod(delegate_cls, sel_act);
            if !m.is_null() {
                method_setImplementation(m, on_activate as *const ());
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn activate_app() {
    unsafe {
        let app_cls = objc_getClass(b"NSApplication\0".as_ptr());
        if !app_cls.is_null() {
            let app = msg_send_ptr(app_cls, "sharedApplication\0");
            if !app.is_null() {
                let sel = sel_registerName(b"activateIgnoringOtherApps:\0".as_ptr());
                let f: extern "C" fn(*mut c_void, *mut c_void, bool) =
                    std::mem::transmute(objc_msgSend as *const ());
                f(app, sel, true);
            }
        }
    }
}

#[cfg(target_os = "macos")]
unsafe fn ns_string(s: &str) -> *mut c_void {
    let str_cls = objc_getClass(b"NSString\0".as_ptr());
    if str_cls.is_null() {
        return std::ptr::null_mut();
    }
    let sel = sel_registerName(b"stringWithUTF8String:\0".as_ptr());
    let mut null_terminated = Vec::with_capacity(s.len() + 1);
    null_terminated.extend_from_slice(s.as_bytes());
    if !s.ends_with('\0') {
        null_terminated.push(0);
    }
    let f: extern "C" fn(*mut c_void, *mut c_void, *const u8) -> *mut c_void =
        std::mem::transmute(objc_msgSend as *const ());
    f(str_cls, sel, null_terminated.as_ptr())
}

#[cfg(target_os = "macos")]
pub fn set_window_represented_filename(window: &Window, path: &str) {
    let Ok(handle) = window.window_handle() else { return };
    let winit::raw_window_handle::RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr() as *mut c_void;
    if ns_view.is_null() {
        return;
    }
    unsafe {
        let ns_window = msg_send_ptr(ns_view, "window\0");
        if ns_window.is_null() {
            return;
        }
        let sel = sel_registerName(b"setRepresentedFilename:\0".as_ptr());
        let str_obj = ns_string(path);
        if !str_obj.is_null() {
            let f: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                std::mem::transmute(objc_msgSend as *const ());
            f(ns_window, sel, str_obj);
        }
    }
}

#[cfg(target_os = "macos")]
#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn objc_getClass(name: *const u8) -> *mut c_void;
    fn sel_registerName(name: *const u8) -> *mut c_void;
    fn objc_msgSend();
    fn class_addMethod(
        cls: *mut c_void,
        name: *mut c_void,
        imp: *const (),
        types: *const u8,
    ) -> bool;
}

#[cfg(not(target_os = "macos"))]
pub fn apply_macos_window_config(_window: &winit::window::Window, _cfg: &crate::config::Config) {}

#[cfg(not(target_os = "macos"))]
pub fn register_reopen_handler(_proxy: winit::event_loop::EventLoopProxy<crate::AppEvent>) {}

#[cfg(not(target_os = "macos"))]
pub fn activate_app() {}

#[cfg(not(target_os = "macos"))]
pub fn set_window_represented_filename(_window: &winit::window::Window, _path: &str) {}
