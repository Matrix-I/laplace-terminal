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
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    use crate::config::{ColorSpace, Config};

    type Id = *mut c_void;

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct NSRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    #[repr(C)]
    #[derive(Copy, Clone, Debug, Default)]
    struct NSPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "objc", kind = "dylib")]
    extern "C" {
        fn objc_getClass(name: *const std::ffi::c_char) -> Id;
        fn sel_registerName(name: *const std::ffi::c_char) -> Id;
        fn objc_msgSend();
        fn class_addMethod(cls: Id, name: Id, imp: *const (), types: *const std::ffi::c_char) -> bool;
        fn class_getInstanceMethod(cls: Id, name: Id) -> Id;
        fn method_setImplementation(m: Id, imp: *const ()) -> *const ();
        fn method_getImplementation(m: Id) -> *const ();
        fn objc_allocateClassPair(superclass: Id, name: *const std::ffi::c_char, extra: usize) -> Id;
        fn objc_registerClassPair(cls: Id);
    }

    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
    }

    #[repr(C)]
    struct EventTypeSpec {
        event_class: u32,
        event_kind: u32,
    }

    #[repr(C)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }

    type EventHandler = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn EnableSecureEventInput() -> i32;
        fn DisableSecureEventInput() -> i32;
        fn GetApplicationEventTarget() -> *mut c_void;
        fn InstallEventHandler(
            target: *mut c_void,
            handler: EventHandler,
            count: u32,
            types: *const EventTypeSpec,
            user_data: *mut c_void,
            out: *mut *mut c_void,
        ) -> i32;
        fn RegisterEventHotKey(
            code: u32,
            modifiers: u32,
            id: EventHotKeyId,
            target: *mut c_void,
            options: u32,
            out: *mut *mut c_void,
        ) -> i32;
        fn UnregisterEventHotKey(hotkey: *mut c_void) -> i32;
    }

    /// The registered Quick Terminal hotkey (`EventHotKeyRef`).
    static HOTKEY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static HOTKEY_HANDLER: AtomicBool = AtomicBool::new(false);

    /// Register (or, with `None`, remove) the system-wide Quick Terminal hotkey.
    /// Needs [`register_reopen_handler`] first, which provides the event proxy.
    pub fn set_global_hotkey(trigger: Option<&crate::keymap::Trigger>) {
        extern "C" fn on_hotkey(_call: *mut c_void, _event: *mut c_void, _data: *mut c_void) -> i32 {
            if let Some(proxy) = REOPEN_PROXY.lock().as_ref() {
                let _ = proxy.send_event(crate::AppEvent::Menu(crate::keymap::Action::ToggleQuickTerminal));
            }
            0
        }
        // SAFETY: Carbon event APIs on the main thread; the handler and the
        // hotkey stay registered until replaced here.
        unsafe {
            let old = HOTKEY.swap(std::ptr::null_mut(), Ordering::Relaxed);
            if !old.is_null() {
                UnregisterEventHotKey(old);
            }
            let Some(t) = trigger else { return };
            let Some(code) = crate::keymap::mac_virtual_keycode(&t.key) else {
                eprintln!("litty: key not supported for a global hotkey");
                return;
            };
            if !HOTKEY_HANDLER.swap(true, Ordering::Relaxed) {
                // kEventClassKeyboard / kEventHotKeyPressed
                let spec = EventTypeSpec { event_class: u32::from_be_bytes(*b"keyb"), event_kind: 5 };
                let target = GetApplicationEventTarget();
                InstallEventHandler(target, on_hotkey, 1, &spec, std::ptr::null_mut(), std::ptr::null_mut());
            }
            // cmdKey, shiftKey, optionKey, controlKey
            let m = t.mods;
            let mods = 0x100 * m.sup as u32 + 0x200 * m.shift as u32 + 0x800 * m.alt as u32 + 0x1000 * m.ctrl as u32;
            let id = EventHotKeyId { signature: u32::from_be_bytes(*b"LTTY"), id: 1 };
            let mut hotkey = std::ptr::null_mut();
            if RegisterEventHotKey(code, mods, id, GetApplicationEventTarget(), 0, &mut hotkey) == 0 {
                HOTKEY.store(hotkey, Ordering::Relaxed);
            } else {
                eprintln!("litty: the Quick Terminal hotkey is taken by another application");
            }
        }
    }

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {
        fn NSBeep();
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceFlagsState(state_id: i32) -> u64;
    }

    static SECURE_INPUT_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Secure Keyboard Entry: other processes cannot observe keystrokes. Calls
    /// are reference-counted by macOS, so every enable needs one disable.
    pub fn set_secure_input(on: bool) {
        use std::sync::atomic::Ordering;
        if SECURE_INPUT_ACTIVE.swap(on, Ordering::SeqCst) == on {
            return;
        }
        // SAFETY: plain Carbon calls without arguments.
        unsafe {
            if on {
                EnableSecureEventInput();
            } else {
                DisableSecureEventInput();
            }
        }
    }

    /// The system alert sound.
    pub fn beep() {
        // SAFETY: plain AppKit call without arguments.
        unsafe { NSBeep() }
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

            install_header_double_click_hook(ns_window);
        }
    }

    static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
    static ORIGINAL_SEND_EVENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static ORIGINAL_ZOOM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static SAVED_FRAMES: parking_lot::Mutex<Option<std::collections::HashMap<usize, NSRect>>> =
        parking_lot::Mutex::new(None);

    fn default_unzoomed_frame(vis: NSRect) -> NSRect {
        let w = (vis.w * 0.70).round().max(600.0).min(vis.w);
        let h = (vis.h * 0.70).round().max(400.0).min(vis.h);
        let x = vis.x + ((vis.w - w) / 2.0).round();
        let y = vis.y + ((vis.h - h) / 2.0).round();
        NSRect { x, y, w, h }
    }

    unsafe fn toggle_window_zoom(this: Id) {
        let screen = send_id(this, c"screen").or_else(|| {
            let sc_cls = class(c"NSScreen")?;
            send_id(sc_cls, c"mainScreen")
        });
        let Some(screen) = screen else { return };

        let visible_frame = msg_send!(screen, c"visibleFrame"; -> NSRect);
        let curr_frame = msg_send!(this, c"frame"; -> NSRect);

        let is_at_visible = (curr_frame.x - visible_frame.x).abs() < 5.0
            && (curr_frame.y - visible_frame.y).abs() < 5.0
            && (curr_frame.w - visible_frame.w).abs() < 5.0
            && (curr_frame.h - visible_frame.h).abs() < 5.0;
        let is_appkit_zoomed = msg_send!(this, c"isZoomed"; -> bool);
        let is_zoomed = is_at_visible || is_appkit_zoomed;

        let mut lock = SAVED_FRAMES.lock();
        let map = lock.get_or_insert_with(std::collections::HashMap::new);
        let key = this as usize;

        if is_zoomed {
            let target = if let Some(saved) = map.remove(&key) {
                let is_saved_zoomed =
                    (saved.w - visible_frame.w).abs() < 5.0 && (saved.h - visible_frame.h).abs() < 5.0;
                if saved.w > 100.0 && saved.h > 100.0 && !is_saved_zoomed {
                    saved
                } else {
                    default_unzoomed_frame(visible_frame)
                }
            } else {
                default_unzoomed_frame(visible_frame)
            };
            msg_send!(this, c"setFrame:display:animate:", target => NSRect, true => bool, true => bool);
        } else {
            map.insert(key, curr_frame);
            msg_send!(this, c"setFrame:display:animate:", visible_frame => NSRect, true => bool, true => bool);
        }
    }

    unsafe fn is_in_window_header(this: Id, event: Id) -> bool {
        let loc = msg_send!(event, c"locationInWindow"; -> NSPoint);
        let frame = msg_send!(this, c"frame"; -> NSRect);

        // Don't intercept when in native full screen mode (NSWindowStyleMaskFullScreen = 1 << 14)
        let style_mask = msg_send!(this, c"styleMask"; -> u64);
        if (style_mask & (1 << 14)) != 0 {
            return false;
        }

        // Left traffic-light buttons (close, minimize, zoom) are within x < 75.0
        if loc.x < 75.0 || loc.x > frame.w {
            return false;
        }

        let content_rect = msg_send!(this, c"contentRectForFrameRect:", frame => NSRect; -> NSRect);
        let header_bottom = if frame.h > content_rect.h {
            content_rect.h - 2.0
        } else {
            frame.h - 38.0
        };

        loc.y >= header_bottom && loc.y <= frame.h + 10.0
    }

    extern "C" fn custom_zoom(this: Id, _cmd: Id, _sender: Id) {
        unsafe {
            toggle_window_zoom(this);
        }
    }

    extern "C" fn custom_send_event(this: Id, cmd: Id, event: Id) {
        if !event.is_null() {
            let is_header_double_click = unsafe {
                // NSEventTypeLeftMouseDown = 1
                let ev_type = msg_send!(event, c"type"; -> usize);
                if ev_type == 1 {
                    let click_count = msg_send!(event, c"clickCount"; -> isize);
                    if click_count == 2 {
                        let modifiers = msg_send!(event, c"modifierFlags"; -> usize);
                        // Ignore if Command (1 << 20) or Control (1 << 18) is held
                        if (modifiers & ((1 << 20) | (1 << 18))) == 0 {
                            is_in_window_header(this, event)
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            };

            if is_header_double_click {
                unsafe {
                    toggle_window_zoom(this);
                }
                return;
            }
        }

        let orig = ORIGINAL_SEND_EVENT.load(Ordering::Relaxed);
        if !orig.is_null() {
            let f: extern "C" fn(Id, Id, Id) = unsafe { std::mem::transmute(orig) };
            f(this, cmd, event);
        }
    }

    unsafe fn install_header_double_click_hook(ns_window: Id) {
        if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }

        let Some(window_cls) = send_id(ns_window, c"class") else { return };

        // Swizzle sendEvent:
        let send_event_sel = sel(c"sendEvent:");
        let send_event_m = class_getInstanceMethod(window_cls, send_event_sel);
        if !send_event_m.is_null() {
            let orig_imp = method_getImplementation(send_event_m);
            let added = class_addMethod(
                window_cls,
                send_event_sel,
                custom_send_event as *const (),
                c"v@:@".as_ptr(),
            );
            let prev = if added {
                orig_imp
            } else {
                method_setImplementation(send_event_m, custom_send_event as *const ())
            };
            ORIGINAL_SEND_EVENT.store(prev as *mut c_void, Ordering::SeqCst);
        }

        // Swizzle zoom:
        let zoom_sel = sel(c"zoom:");
        let zoom_m = class_getInstanceMethod(window_cls, zoom_sel);
        if !zoom_m.is_null() {
            let orig_imp = method_getImplementation(zoom_m);
            let added = class_addMethod(
                window_cls,
                zoom_sel,
                custom_zoom as *const (),
                c"v@:@".as_ptr(),
            );
            let prev = if added {
                orig_imp
            } else {
                method_setImplementation(zoom_m, custom_zoom as *const ())
            };
            ORIGINAL_ZOOM.store(prev as *mut c_void, Ordering::SeqCst);
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

    /// Background blur: an NSVisualEffectView behind the window content.
    unsafe fn setup_visual_effect_view(ns_window: Id) {
        let Some(content_view) = send_id(ns_window, c"contentView") else { return };
        // Avoid duplicate effect views if config is re-applied to the same window
        if let Some(effect_cls) = class(c"NSVisualEffectView") {
            let mut already_has_blur = false;
            if let Some(subviews) = send_id(content_view, c"subviews") {
                for_each_in_array(subviews, |subview| {
                    if msg_send!(subview, c"isKindOfClass:", effect_cls => Id; -> bool) {
                        already_has_blur = true;
                    }
                });
            }
            if already_has_blur {
                return;
            }
        }
        let Some(alloc) = class(c"NSVisualEffectView").and_then(|cls| send_id(cls, c"alloc")) else { return };

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
        msg_send!(effect_view, c"release");
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
        observe_system_appearance();
    }

    /// Windows carry their own NSAppearance (matching the terminal background),
    /// so winit's ThemeChanged never fires. The application's appearance still
    /// follows the system: watch it with KVO (the change is reported after it
    /// took effect, so `system_dark` already sees the new value).
    fn observe_system_appearance() {
        static OBSERVER_SETUP: AtomicBool = AtomicBool::new(false);
        if OBSERVER_SETUP.swap(true, Ordering::SeqCst) {
            return;
        }

        extern "C" fn on_change(_this: Id, _cmd: Id, _path: Id, _object: Id, _change: Id, _context: *mut c_void) {
            if let Some(proxy) = REOPEN_PROXY.lock().as_ref() {
                let _ = proxy.send_event(crate::AppEvent::SystemAppearance { dark: system_dark() });
            }
        }
        // SAFETY: the observer class is created once with a method matching its
        // type encoding; the observer instance lives for the whole process.
        unsafe {
            let Some(app) = shared_application() else { return };
            let name = c"LittyAppearanceObserver";
            let mut cls = objc_getClass(name.as_ptr());
            if cls.is_null() {
                let Some(base) = class(c"NSObject") else { return };
                cls = objc_allocateClassPair(base, name.as_ptr(), 0);
                if cls.is_null() {
                    return;
                }
                let selector = sel(c"observeValueForKeyPath:ofObject:change:context:");
                class_addMethod(cls, selector, on_change as *const (), c"v@:@@@^v".as_ptr());
                objc_registerClassPair(cls);
            }
            let Some(observer) = send_id(cls, c"alloc").and_then(|o| send_id(o, c"init")) else { return };
            let Some(key) = ns_string("effectiveAppearance") else { return };
            // NSKeyValueObservingOptionNew = 1
            msg_send!(
                app,
                c"addObserver:forKeyPath:options:context:",
                observer => Id,
                key => Id,
                1 => usize,
                std::ptr::null_mut::<c_void>() => *mut c_void
            );
        }
    }

    /// The system appearance is Dark: the application's effective appearance
    /// (Litty never sets one on NSApp, only on its windows).
    pub fn system_dark() -> bool {
        // SAFETY: NSApp and its appearance are live; the name is read while
        // the autoreleased objects are still alive.
        unsafe {
            let name = shared_application()
                .and_then(|app| send_id(app, c"effectiveAppearance"))
                .and_then(|appearance| send_id(appearance, c"name"))
                .map(|name| msg_send!(name, c"UTF8String"; -> *const std::ffi::c_char))
                .filter(|p| !p.is_null());
            name.is_none_or(|p| CStr::from_ptr(p).to_bytes().windows(4).any(|w| w == b"Dark"))
        }
    }

    use crate::keymap::Action;

    /// A menu entry: one of Litty's own actions (sent to the event loop), an
    /// AppKit selector sent along the responder chain, or a separator. Key
    /// equivalents are only given where AppKit must not see the key first;
    /// every other shortcut stays in the (rebindable) keymap.
    enum Item {
        Litty(&'static str, Action),
        Native(&'static str, &'static CStr, &'static str, u64),
        Separator,
    }

    const CMD: u64 = 1 << 20;
    const OPT: u64 = 1 << 19;
    const CTRL: u64 = 1 << 18;

    /// Actions reachable from the menu; a menu item's tag indexes this list.
    const MENU_ACTIONS: [Action; 17] = [
        Action::Quit,
        Action::ToggleSecureInput,
        Action::NewWindow,
        Action::NewTab,
        Action::SplitRight,
        Action::SplitDown,
        Action::CloseTab,
        Action::Copy,
        Action::Paste,
        Action::SelectAll,
        Action::Find,
        Action::ClearScreen,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::ZoomReset,
        Action::OpenConfig,
        Action::CheckUpdate,
    ];

    /// The "Secure Keyboard Entry" item, to show its check mark.
    static SECURE_ITEM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    fn menu_tag(action: Action) -> isize {
        MENU_ACTIONS.iter().position(|&a| a == action).map_or(-1, |i| i as isize)
    }

    /// Replace winit's default menu (whose Quit calls `terminate:` directly,
    /// skipping `confirm-close-surface`) with Litty's. Call after
    /// [`register_reopen_handler`], which provides the event loop proxy.
    pub fn install_menu() {
        extern "C" fn on_menu(_this: Id, _cmd: Id, sender: Id) {
            // SAFETY: `sender` is the NSMenuItem that was chosen.
            let tag = unsafe { msg_send!(sender, c"tag"; -> isize) };
            let action = usize::try_from(tag).ok().and_then(|i| MENU_ACTIONS.get(i));
            if let (Some(&action), Some(proxy)) = (action, REOPEN_PROXY.lock().as_ref()) {
                let _ = proxy.send_event(crate::AppEvent::Menu(action));
            }
        }

        use Item::{Litty, Native, Separator};
        let menus: [(&str, Vec<Item>); 5] = [
            (
                "Litty",
                vec![
                    Native("About Litty", c"orderFrontStandardAboutPanel:", "", 0),
                    Separator,
                    Litty("Settings…", Action::OpenConfig),
                    Litty("Check for Updates…", Action::CheckUpdate),
                    Litty("Secure Keyboard Entry", Action::ToggleSecureInput),
                    Separator,
                    Native("Hide Litty", c"hide:", "h", CMD),
                    Native("Hide Others", c"hideOtherApplications:", "h", CMD | OPT),
                    Native("Show All", c"unhideAllApplications:", "", 0),
                    Separator,
                    Litty("Quit Litty", Action::Quit),
                ],
            ),
            (
                "Shell",
                vec![
                    Litty("New Window", Action::NewWindow),
                    Litty("New Tab", Action::NewTab),
                    Separator,
                    Litty("Split Right", Action::SplitRight),
                    Litty("Split Down", Action::SplitDown),
                    Separator,
                    Litty("Close", Action::CloseTab),
                ],
            ),
            (
                "Edit",
                vec![
                    Litty("Copy", Action::Copy),
                    Litty("Paste", Action::Paste),
                    Litty("Select All", Action::SelectAll),
                    Separator,
                    Litty("Find…", Action::Find),
                    Litty("Clear Screen", Action::ClearScreen),
                ],
            ),
            (
                "View",
                vec![
                    Litty("Bigger", Action::ZoomIn),
                    Litty("Smaller", Action::ZoomOut),
                    Litty("Actual Size", Action::ZoomReset),
                    Separator,
                    Native("Enter Full Screen", c"toggleFullScreen:", "f", CMD | CTRL),
                ],
            ),
            (
                "Window",
                vec![Native("Minimize", c"performMiniaturize:", "", 0), Native("Zoom", c"performZoom:", "", 0)],
            ),
        ];

        // SAFETY: standard Objective-C runtime and AppKit calls; the target
        // class and every menu object are created here and live for the
        // whole process.
        unsafe {
            let Some(app) = shared_application() else { return };
            let target = {
                let name = c"LittyMenuTarget";
                let mut cls = objc_getClass(name.as_ptr());
                if cls.is_null() {
                    let Some(base) = class(c"NSObject") else { return };
                    cls = objc_allocateClassPair(base, name.as_ptr(), 0);
                    if cls.is_null() {
                        return;
                    }
                    class_addMethod(cls, sel(c"littyMenu:"), on_menu as *const (), c"v@:@".as_ptr());
                    objc_registerClassPair(cls);
                }
                let Some(obj) = send_id(cls, c"alloc").and_then(|o| send_id(o, c"init")) else { return };
                obj
            };
            let (Some(menu_cls), Some(item_cls)) = (class(c"NSMenu"), class(c"NSMenuItem")) else { return };
            let new_menu = |title: &str| -> Option<Id> {
                let menu = send_id(menu_cls, c"alloc")?;
                Some(msg_send!(menu, c"initWithTitle:", ns_string(title)? => Id; -> Id))
            };
            let new_item = |title: &str, action: Id, key: &str| -> Option<Id> {
                let item = send_id(item_cls, c"alloc")?;
                let item = msg_send!(
                    item,
                    c"initWithTitle:action:keyEquivalent:",
                    ns_string(title)? => Id,
                    action => Id,
                    ns_string(key)? => Id;
                    -> Id
                );
                (!item.is_null()).then_some(item)
            };

            let Some(main_menu) = new_menu("") else { return };
            for (title, items) in menus {
                let Some(submenu) = new_menu(title) else { continue };
                for item in items {
                    let entry = match item {
                        Separator => send_id(item_cls, c"separatorItem"),
                        Litty(label, action) => {
                            // Standard macOS shortcuts: Cmd+Q for Quit, Cmd+, for Settings.
                            let key = match action {
                                Action::Quit => "q",
                                Action::OpenConfig => ",",
                                _ => "",
                            };
                            new_item(label, sel(c"littyMenu:"), key).inspect(|&it| {
                                msg_send!(it, c"setTarget:", target => Id);
                                msg_send!(it, c"setTag:", menu_tag(action) => isize);
                                if action == Action::ToggleSecureInput {
                                    SECURE_ITEM.store(it, Ordering::Relaxed);
                                }
                            })
                        }
                        Native(label, selector, key, mods) => new_item(label, sel(selector), key).inspect(|&it| {
                            if mods != 0 {
                                msg_send!(it, c"setKeyEquivalentModifierMask:", mods => u64);
                            }
                        }),
                    };
                    if let Some(entry) = entry {
                        msg_send!(submenu, c"addItem:", entry => Id);
                    }
                }
                let Some(top) = new_item(title, std::ptr::null_mut(), "") else { continue };
                msg_send!(top, c"setSubmenu:", submenu => Id);
                msg_send!(main_menu, c"addItem:", top => Id);
                if title == "Window" {
                    msg_send!(app, c"setWindowsMenu:", submenu => Id);
                }
            }
            msg_send!(app, c"setMainMenu:", main_menu => Id);
        }
    }

    /// Hide the app, giving focus back to the previously active application.
    pub fn hide_app() {
        // SAFETY: standard NSApplication call.
        unsafe {
            if let Some(app) = shared_application() {
                msg_send!(app, c"hide:", std::ptr::null_mut::<c_void>() => Id);
            }
        }
    }

    /// Attach `child` as a native tab of `parent`, right after it.
    pub fn add_tabbed_window(parent: &Window, child: &Window) {
        set_tabbing_allowed(parent, true);
        set_tabbing_allowed(child, true);
        let (Some((_, p)), Some((_, c))) = (ns_window_of(parent), ns_window_of(child)) else { return };
        // SAFETY: both are live NSWindows; 1 = NSWindowAbove.
        unsafe { msg_send!(p, c"addTabbedWindow:ordered:", c => Id, 1 => isize) };
    }

    /// Let `window` join native tab groups (`true`), or keep it a separate
    /// window even where AppKit would merge it into a tab of another Litty
    /// window (`false`). Takes effect the next time the window is ordered in.
    pub fn set_tabbing_allowed(window: &Window, allowed: bool) {
        let Some((_, w)) = ns_window_of(window) else { return };
        // NSWindowTabbingMode: 0 = automatic, 2 = disallowed.
        let mode: isize = if allowed { 0 } else { 2 };
        // SAFETY: live NSWindow.
        unsafe { msg_send!(w, c"setTabbingMode:", mode => isize) };
    }

    /// Move between native tabs of `window`: -1 previous, 1 next, or index
    /// `n` (8 = last). Returns `false` when the window has no other tabs.
    pub fn select_native_tab(window: &Window, step: isize, index: Option<usize>) -> bool {
        use winit::platform::macos::WindowExtMacOS;
        let n = window.num_tabs();
        if n < 2 {
            return false;
        }
        match (index, step) {
            (Some(8), _) => window.select_tab_at_index(n - 1),
            (Some(i), _) if i < n => window.select_tab_at_index(i),
            (Some(_), _) => return false,
            (None, s) if s < 0 => window.select_previous_tab(),
            (None, _) => window.select_next_tab(),
        }
        true
    }

    /// Caps Lock is on: the hardware state (kCGEventSourceStateHIDSystemState,
    /// kCGEventFlagMaskAlphaShift), which inside the app is not tied to how far
    /// AppKit's event queue has got.
    pub fn caps_lock() -> bool {
        // SAFETY: plain C call without pointers.
        unsafe { CGEventSourceFlagsState(1) & 0x1_0000 != 0 }
    }

    /// Show or clear the check mark of the "Secure Keyboard Entry" menu item.
    pub fn set_secure_input_menu_state(on: bool) {
        let item = SECURE_ITEM.load(Ordering::Relaxed);
        if !item.is_null() {
            // SAFETY: the item lives as long as the menu, i.e. the process.
            unsafe { msg_send!(item, c"setState:", isize::from(on) => isize) };
        }
    }

    pub fn set_dock_icon() {
        // SAFETY: standard NSApplication / NSImage calls on live objects.
        unsafe {
            let Some(app) = shared_application() else { return };

            // Inside a .app bundle the Dock already reads Litty.icns (or Laplace.icns) from the bundle;
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

    /// Ask with a modal alert; `true` when `button` (the default) is chosen.
    /// Without AppKit available, answers `true`.
    pub fn confirm(title: &str, text: &str, button: &str) -> bool {
        // SAFETY: standard NSAlert calls on an object we allocated; `runModal`
        // spins a nested run loop, during which winit queues its events.
        unsafe {
            let Some(cls) = class(c"NSAlert") else { return true };
            let Some(alert) = send_id(cls, c"alloc").and_then(|a| send_id(a, c"init")) else { return true };
            for (selector, value) in [(c"setMessageText:", title), (c"setInformativeText:", text)] {
                if let Some(s) = ns_string(value) {
                    msg_send!(alert, selector, s => Id);
                }
            }
            for label in [button, "Cancel"] {
                if let Some(s) = ns_string(label) {
                    msg_send!(alert, c"addButtonWithTitle:", s => Id);
                }
            }
            activate_app();
            // NSAlertFirstButtonReturn
            let answer = msg_send!(alert, c"runModal"; -> isize);
            msg_send!(alert, c"release");
            answer == 1000
        }
    }

    /// Present a modal alert with arbitrary button labels. Returns the index of
    /// the selected button (0 = first button, 1 = second, ...).
    pub fn alert_choice(title: &str, text: &str, buttons: &[&str]) -> usize {
        unsafe {
            let Some(cls) = class(c"NSAlert") else { return 0 };
            let Some(alert) = send_id(cls, c"alloc").and_then(|a| send_id(a, c"init")) else { return 0 };
            for (selector, value) in [(c"setMessageText:", title), (c"setInformativeText:", text)] {
                if let Some(s) = ns_string(value) {
                    msg_send!(alert, selector, s => Id);
                }
            }
            for &label in buttons {
                if let Some(s) = ns_string(label) {
                    msg_send!(alert, c"addButtonWithTitle:", s => Id);
                }
            }
            activate_app();
            let answer = msg_send!(alert, c"runModal"; -> isize);
            msg_send!(alert, c"release");
            if answer >= 1000 {
                (answer - 1000) as usize
            } else {
                0
            }
        }
    }

    /// Present an informational modal alert with an "OK" button.
    pub fn alert(title: &str, text: &str) {
        let _ = alert_choice(title, text, &["OK"]);
    }

    static ACTIVE_PROGRESS_WINDOW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static PROGRESS_DELEGATE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn progress_window_will_close(_this: *mut c_void, _cmd: *mut c_void, _notification: *mut c_void) {
        let win = ACTIVE_PROGRESS_WINDOW.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !win.is_null() {
            unsafe {
                msg_send!(win, c"release");
            }
        }
    }

    fn progress_delegate() -> Option<Id> {
        let cur = PROGRESS_DELEGATE.load(Ordering::SeqCst);
        if !cur.is_null() {
            return Some(cur);
        }
        unsafe {
            let name = c"LittyProgressDelegate";
            let mut cls = objc_getClass(name.as_ptr());
            if cls.is_null() {
                let base = class(c"NSObject")?;
                cls = objc_allocateClassPair(base, name.as_ptr(), 0);
                if cls.is_null() {
                    return None;
                }
                class_addMethod(cls, sel(c"windowWillClose:"), progress_window_will_close as *const (), c"v@:@".as_ptr());
                objc_registerClassPair(cls);
            }
            let obj = send_id(cls, c"alloc").and_then(|o| send_id(o, c"init"))?;
            PROGRESS_DELEGATE.store(obj, Ordering::SeqCst);
            Some(obj)
        }
    }

    /// Show a modeless progress dialog with an indeterminate progress bar.
    /// Must be called on the main thread.
    pub fn show_progress_dialog(title: &str, text: &str) {
        unsafe {
            close_progress_dialog();

            let Some(win_cls) = class(c"NSWindow") else { return };
            let Some(alloc) = send_id(win_cls, c"alloc") else { return };
            let frame = NSRect { x: 0.0, y: 0.0, w: 360.0, h: 90.0 };
            // NSWindowStyleMaskTitled (1), NSBackingStoreBuffered (2), defer: false (0)
            let win = msg_send!(
                alloc,
                c"initWithContentRect:styleMask:backing:defer:",
                frame => NSRect, 1u64 => u64, 2u64 => u64, false => bool;
                -> Id
            );
            if win.is_null() {
                return;
            }

            if let Some(s) = ns_string("Litty") {
                msg_send!(win, c"setTitle:", s => Id);
            }
            msg_send!(win, c"setReleasedWhenClosed:", false => bool);
            if let Some(del) = progress_delegate() {
                msg_send!(win, c"setDelegate:", del => Id);
            }

            let Some(content) = send_id(win, c"contentView") else {
                msg_send!(win, c"release");
                return;
            };

            // App icon on the left
            let app_icon = shared_application().and_then(|app| send_id(app, c"applicationIconImage"));
            if let Some(iv_cls) = class(c"NSImageView") {
                if let Some(alloc_iv) = send_id(iv_cls, c"alloc") {
                    let iv_frame = NSRect { x: 18.0, y: 20.0, w: 48.0, h: 48.0 };
                    let iv = msg_send!(alloc_iv, c"initWithFrame:", iv_frame => NSRect; -> Id);
                    if !iv.is_null() {
                        if let Some(img) = app_icon {
                            msg_send!(iv, c"setImage:", img => Id);
                        }
                        msg_send!(content, c"addSubview:", iv => Id);
                        msg_send!(iv, c"release");
                    }
                }
            }

            // Title label
            if let Some(tf_cls) = class(c"NSTextField") {
                if let Some(alloc_tf) = send_id(tf_cls, c"alloc") {
                    let tf_frame = NSRect { x: 78.0, y: 55.0, w: 265.0, h: 20.0 };
                    let tf = msg_send!(alloc_tf, c"initWithFrame:", tf_frame => NSRect; -> Id);
                    if !tf.is_null() {
                        if let Some(s) = ns_string(title) {
                            msg_send!(tf, c"setStringValue:", s => Id);
                        }
                        msg_send!(tf, c"setBezeled:", false => bool);
                        msg_send!(tf, c"setDrawsBackground:", false => bool);
                        msg_send!(tf, c"setEditable:", false => bool);
                        msg_send!(tf, c"setSelectable:", false => bool);
                        if let Some(font_cls) = class(c"NSFont") {
                            let bold_font: Id = msg_send!(font_cls, c"boldSystemFontOfSize:", 13.0f64 => f64; -> Id);
                            if !bold_font.is_null() {
                                msg_send!(tf, c"setFont:", bold_font => Id);
                            }
                        }
                        msg_send!(content, c"addSubview:", tf => Id);
                        msg_send!(tf, c"release");
                    }
                }
            }

            // Informational text label
            if let Some(tf_cls) = class(c"NSTextField") {
                if let Some(alloc_tf) = send_id(tf_cls, c"alloc") {
                    let tf_frame = NSRect { x: 78.0, y: 35.0, w: 265.0, h: 18.0 };
                    let tf = msg_send!(alloc_tf, c"initWithFrame:", tf_frame => NSRect; -> Id);
                    if !tf.is_null() {
                        if let Some(s) = ns_string(text) {
                            msg_send!(tf, c"setStringValue:", s => Id);
                        }
                        msg_send!(tf, c"setBezeled:", false => bool);
                        msg_send!(tf, c"setDrawsBackground:", false => bool);
                        msg_send!(tf, c"setEditable:", false => bool);
                        msg_send!(tf, c"setSelectable:", false => bool);
                        if let Some(font_cls) = class(c"NSFont") {
                            let sys_font: Id = msg_send!(font_cls, c"systemFontOfSize:", 11.0f64 => f64; -> Id);
                            if !sys_font.is_null() {
                                msg_send!(tf, c"setFont:", sys_font => Id);
                            }
                        }
                        msg_send!(content, c"addSubview:", tf => Id);
                        msg_send!(tf, c"release");
                    }
                }
            }

            // Indeterminate progress bar
            if let Some(ind_cls) = class(c"NSProgressIndicator") {
                if let Some(alloc_ind) = send_id(ind_cls, c"alloc") {
                    let frame = NSRect { x: 78.0, y: 15.0, w: 265.0, h: 14.0 };
                    let ind = msg_send!(alloc_ind, c"initWithFrame:", frame => NSRect; -> Id);
                    if !ind.is_null() {
                        msg_send!(ind, c"setStyle:", 1u64 => u64); // NSProgressIndicatorStyleBar = 1
                        msg_send!(ind, c"setIndeterminate:", true => bool);
                        msg_send!(ind, c"startAnimation:", std::ptr::null_mut() => Id);
                        msg_send!(content, c"addSubview:", ind => Id);
                        msg_send!(ind, c"release");
                    }
                }
            }

            msg_send!(win, c"center");
            msg_send!(win, c"makeKeyAndOrderFront:", std::ptr::null_mut() => Id);
            activate_app();

            ACTIVE_PROGRESS_WINDOW.store(win, Ordering::SeqCst);
        }
    }

    /// Close the active modeless progress dialog if one is shown.
    /// Must be called on the main thread.
    pub fn close_progress_dialog() {
        unsafe {
            let win = ACTIVE_PROGRESS_WINDOW.swap(std::ptr::null_mut(), Ordering::SeqCst);
            if !win.is_null() {
                msg_send!(win, c"setDelegate:", std::ptr::null_mut() => Id);
                msg_send!(win, c"orderOut:", std::ptr::null_mut() => Id);
                msg_send!(win, c"close");
                msg_send!(win, c"release");
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_default_unzoomed_frame() {
            let vis = NSRect { x: 0.0, y: 25.0, w: 1512.0, h: 957.0 };
            let unzoomed = default_unzoomed_frame(vis);
            assert!(unzoomed.w >= 600.0 && unzoomed.w <= vis.w);
            assert!(unzoomed.h >= 400.0 && unzoomed.h <= vis.h);
            assert!(unzoomed.x >= vis.x);
            assert!(unzoomed.y >= vis.y);
        }

        #[test]
        fn test_saved_frames_cycle() {
            let mut lock = SAVED_FRAMES.lock();
            let map = lock.get_or_insert_with(std::collections::HashMap::new);
            let test_id = 0x12345usize;
            let original_frame = NSRect { x: 100.0, y: 100.0, w: 900.0, h: 600.0 };
            
            // Save frame on zoom
            map.insert(test_id, original_frame);
            assert_eq!(map.len(), 1);

            // Pop frame on unzoom
            let restored = map.remove(&test_id).unwrap();
            assert_eq!(restored.x, 100.0);
            assert_eq!(restored.y, 100.0);
            assert_eq!(restored.w, 900.0);
            assert_eq!(restored.h, 600.0);
            assert!(map.get(&test_id).is_none());
        }
    }
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
mod stub {
    pub fn release_free_memory() {}
    pub fn apply_macos_window_config(_window: &winit::window::Window, _cfg: &crate::config::Config) {}
    pub fn register_reopen_handler(_proxy: winit::event_loop::EventLoopProxy<crate::AppEvent>) {}
    pub fn install_menu() {}
    pub fn set_global_hotkey(_trigger: Option<&crate::keymap::Trigger>) {}
    pub fn hide_app() {}
    pub fn add_tabbed_window(_parent: &winit::window::Window, _child: &winit::window::Window) {}
    pub fn set_tabbing_allowed(_window: &winit::window::Window, _allowed: bool) {}
    pub fn select_native_tab(_window: &winit::window::Window, _step: isize, _index: Option<usize>) -> bool {
        false
    }
    pub fn caps_lock() -> bool {
        false
    }
    pub fn system_dark() -> bool {
        true
    }
    pub fn set_secure_input_menu_state(_on: bool) {}
    pub fn set_dock_icon() {}
    pub fn activate_app() {}
    pub fn set_window_represented_filename(_window: &winit::window::Window, _path: &str) {}
    pub fn set_secure_input(_on: bool) {}
    pub fn beep() {}
    pub fn show_progress_dialog(_title: &str, _text: &str) {}
    pub fn close_progress_dialog() {}
    pub fn alert_choice(title: &str, text: &str, buttons: &[&str]) -> usize {
        let zenity_bin = if std::path::Path::new("/usr/bin/zenity").exists() { "/usr/bin/zenity" } else { "zenity" };
        let mut cmd = std::process::Command::new(zenity_bin);
        cmd.args(["--question", "--title", title, "--text", text]);
        if let Some(first) = buttons.first() {
            cmd.args(["--ok-label", first]);
        }
        if let Some(second) = buttons.get(1) {
            cmd.args(["--cancel-label", second]);
        }
        match cmd.status() {
            Ok(status) if status.success() => 0,
            _ => 1,
        }
    }
    pub fn confirm(title: &str, text: &str, button: &str) -> bool {
        alert_choice(title, text, &[button, "Cancel"]) == 0
    }
    pub fn alert(title: &str, text: &str) {
        let zenity_bin = if std::path::Path::new("/usr/bin/zenity").exists() { "/usr/bin/zenity" } else { "zenity" };
        let _ = std::process::Command::new(zenity_bin)
            .args(["--info", "--title", title, "--text", text])
            .status();
    }
}
