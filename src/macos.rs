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
            }
        }

        // 2. Opacity and background color
        if cfg.background_opacity < 1.0 {
            let set_opaque_sel = sel_registerName(b"setOpaque:\0".as_ptr());
            let f: extern "C" fn(*mut c_void, *mut c_void, bool) =
                std::mem::transmute(objc_msgSend as *const ());
            f(ns_window, set_opaque_sel, false);

            let nscolor_cls = objc_getClass(b"NSColor\0".as_ptr());
            let clear_col = msg_send_ptr(nscolor_cls, "clearColor\0");
            if !clear_col.is_null() {
                let set_bg_sel = sel_registerName(b"setBackgroundColor:\0".as_ptr());
                let f: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                    std::mem::transmute(objc_msgSend as *const ());
                f(ns_window, set_bg_sel, clear_col);
            }
        }

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
#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn objc_getClass(name: *const u8) -> *mut c_void;
    fn sel_registerName(name: *const u8) -> *mut c_void;
    fn objc_msgSend();
}

#[cfg(not(target_os = "macos"))]
pub fn apply_macos_window_config(_window: &winit::window::Window, _cfg: &crate::config::Config) {}
