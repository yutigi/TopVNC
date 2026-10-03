//! macOS desktop host for `topvnc --serve` and the Server tab: ScreenCaptureKit
//! capture, Quartz event injection, and pasteboard sync.

use crate::desktop_host::{
    CAPTURE_RETRY_MAX, CAPTURE_RETRY_MIN, CaptureSurface, ClickTracker, DisplayBounds, FrameDamage,
    FrameSlot, HostPermissions, KeyIdentity, MAC_FLAG_ALPHA_SHIFT, MAX_CLIPBOARD_CHARS, MacKeyCode,
    MouseMode, PointerTransition, Rect, RemoteInputState, Rotation, ServeOptions, ServerNotice,
    capture_rate, display_point, frame_damage, latin1_from_unicode, latin1_to_string,
    macos_event_flags, macos_key_flags, macos_key_identity, macos_modifier_flags,
    parse_serve_arguments, relative_point, served_size, unicode_key_units,
    validate_capture_dimensions,
};
use block2::RcBlock;
use dispatch2::{DispatchQoS, DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSPasteboard, NSPasteboardTypeString, NSScreen};
use objc2_core_foundation::{
    CFDictionary, CFRetained, CFRunLoop, CGPoint, CGRect, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayChangeSummaryFlags, CGDisplayCopyDisplayMode,
    CGDisplayMode, CGDisplayRegisterReconfigurationCallback,
    CGDisplayRemoveReconfigurationCallback, CGEvent, CGEventField, CGEventFlags, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMainDisplayID, CGMouseButton,
    CGPreflightScreenCaptureAccess, CGRectMakeWithDictionaryRepresentation,
    CGRequestScreenCaptureAccess, CGScrollEventUnit,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{CVPixelBuffer, CVPixelBufferGetIOSurface, kCVPixelFormatType_32BGRA};
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSNumber, NSOperatingSystemVersion, NSProcessInfo, NSString,
    ns_string,
};
use objc2_io_surface::IOSurfaceLockOptions;
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamFrameInfoDirtyRects, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType,
};
use std::error::Error;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use topvnc::{
    BUTTON_BACK, BUTTON_FORWARD, BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, ClientEvent, DamageRect,
    Framebuffer, ServerConfig, VncServer,
};

static SERVER_SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Set by the display reconfiguration callback; one server runs per process.
static DISPLAY_RECONFIGURED: AtomicBool = AtomicBool::new(false);

/// ScreenCaptureKit first shipped in macOS 12.3.
const MINIMUM_MACOS: (isize, isize) = (12, 3);
/// How long the serving loop waits for a frame before other housekeeping.
const FRAME_WAIT: Duration = Duration::from_millis(50);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
/// Bounds every ScreenCaptureKit completion handler wait.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(10);
/// Frames ScreenCaptureKit may have in flight: one in the hand-off slot, one
/// being copied, and one being captured.
const STREAM_QUEUE_DEPTH: isize = 3;
/// Display size checks in case a reconfiguration callback is not delivered.
const DISPLAY_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const PASTEBOARD_POLL_INTERVAL: Duration = Duration::from_millis(250);
const ACCESSIBILITY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DISPLAY_NAME_TIMEOUT: Duration = Duration::from_millis(500);
const SCREEN_RECORDING_SETTINGS: &str =
    "System Settings → Privacy & Security → Screen & System Audio Recording";
const ACCESSIBILITY_SETTINGS: &str = "System Settings → Privacy & Security → Accessibility";

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: &'static NSString;
}

extern "C" fn request_shutdown(_signal: c_int) {
    SERVER_SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

/// Run `topvnc --serve` until interrupted.
pub fn run(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let options = parse_serve_arguments(arguments)?;
    let password = if options.allow_insecure {
        eprintln!("WARNING: RFB clients will be unauthenticated. Bind only to a trusted network.");
        None
    } else {
        let password = rpassword::prompt_password("VNC server password: ")?;
        if password.is_empty() {
            return Err("server password must not be empty".into());
        }
        Some(password)
    };
    SERVER_SHUTDOWN_REQUESTED.store(false, Ordering::Release);
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let handler = request_shutdown as extern "C" fn(c_int) as libc::sighandler_t;
        if unsafe { libc::signal(signal, handler) } == libc::SIG_ERR {
            return Err("could not register the server shutdown handler".into());
        }
    }
    // The main thread runs its run loop so display reconfiguration callbacks
    // and main-queue work (display names) are delivered while serving.
    let server = thread::Builder::new()
        .name("topvnc-server".into())
        .spawn(move || {
            serve(
                options,
                password,
                &SERVER_SHUTDOWN_REQUESTED,
                &|notice| match notice {
                    ServerNotice::Connections(_) => {}
                    notice => eprintln!("{notice}"),
                },
            )
            .map_err(|error| error.to_string())
        })?;
    while !server.is_finished() {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.1, false);
    }
    server
        .join()
        .map_err(|_| "server thread panicked")?
        .map_err(Into::into)
}

/// Whether this process may capture the screen and inject input. Never
/// prompts; the Server tab polls it while the server is stopped.
pub fn permissions() -> HostPermissions {
    HostPermissions {
        screen_recording: CGPreflightScreenCaptureAccess(),
        accessibility: unsafe { AXIsProcessTrusted() } != 0,
    }
}

/// Who macOS grants permissions to: a process started from a terminal
/// inherits the terminal app's permissions.
fn permission_holder() -> String {
    match std::env::var("TERM_PROGRAM") {
        Ok(terminal) if !terminal.is_empty() => {
            format!("the terminal app that started TopVNC ({terminal})")
        }
        _ => "TopVNC (or the terminal app that started it)".into(),
    }
}

fn require_supported_macos() -> Result<(), String> {
    let info = NSProcessInfo::processInfo();
    let minimum = NSOperatingSystemVersion {
        majorVersion: MINIMUM_MACOS.0,
        minorVersion: MINIMUM_MACOS.1,
        patchVersion: 0,
    };
    if info.isOperatingSystemAtLeastVersion(minimum) {
        return Ok(());
    }
    let running = info.operatingSystemVersion();
    Err(format!(
        "sharing this Mac requires macOS {}.{} or later (ScreenCaptureKit); this Mac runs macOS {}.{}.{}",
        MINIMUM_MACOS.0,
        MINIMUM_MACOS.1,
        running.majorVersion,
        running.minorVersion,
        running.patchVersion
    ))
}

fn require_screen_recording() -> Result<(), String> {
    if CGPreflightScreenCaptureAccess() {
        return Ok(());
    }
    // Shows the system prompt or adds the app to the settings list.
    CGRequestScreenCaptureAccess();
    let holder = permission_holder();
    Err(format!(
        "Screen Recording access is required. Allow {holder} in {SCREEN_RECORDING_SETTINGS}, \
         then quit and relaunch it: macOS applies the change only after a relaunch"
    ))
}

/// Check Accessibility access, showing the system prompt if it is missing.
fn accessibility_trusted_prompting() -> bool {
    let options = NSDictionary::<NSString, AnyObject>::from_slices(
        &[unsafe { kAXTrustedCheckOptionPrompt }],
        &[NSNumber::new_bool(true).as_ref()],
    );
    let options: *const NSDictionary<NSString, AnyObject> = &*options;
    unsafe { AXIsProcessTrustedWithOptions(options.cast()) != 0 }
}

/// Where the served display is, for placing remote pointer events.
#[derive(Debug, Clone, Copy)]
struct InputPlacement {
    display: CGDirectDisplayID,
    /// The served framebuffer size viewer coordinates refer to.
    width: u16,
    height: u16,
}

/// Capture and serve a display until `shutdown` is set. Runs on the calling
/// thread, which must not be the main thread; `report` receives progress
/// from the capture and input threads.
pub fn serve(
    options: ServeOptions,
    password: Option<String>,
    shutdown: &AtomicBool,
    report: &(dyn Fn(ServerNotice) + Sync),
) -> Result<(), Box<dyn Error>> {
    let ServeOptions {
        address,
        display,
        allow_insecure,
        scale,
        mouse,
        foveation,
    } = options;
    require_supported_macos()?;
    require_screen_recording()?;
    interactive_qos();
    let input_allowed = accessibility_trusted_prompting();
    let _reconfiguration = ReconfigurationCallback::register();
    let capture = DisplayCapture::start(display, scale)?;
    let mut framebuffer = Framebuffer::new(capture.width, capture.height)?;
    let started = Instant::now();
    while started.elapsed() < FIRST_FRAME_TIMEOUT {
        // A frame that cannot be used, such as one from just before a display
        // change, leaves the first image to the serving loop.
        if let Some((buffer, damage)) = capture.shared.frames.take_timeout(FRAME_WAIT) {
            if copy_frame(&buffer, &damage, &mut framebuffer).is_err() {
                capture.request_full_frame();
            }
            break;
        }
        if let Some(error) = capture.stopped_error() {
            return Err(format!("display capture stopped: {error}").into());
        }
    }
    let config = ServerConfig {
        name: "TopVNC macOS Desktop".into(),
        password,
        allow_insecure,
        // Session, input, and encoder threads do not inherit QoS.
        thread_setup: Some(interactive_qos),
        foveation,
    };
    let server = VncServer::bind(&address, framebuffer.clone(), config)
        .map_err(|error| bind_error(error, &address))?;
    // macOS has no system-wide cursor visibility to follow, so only an
    // explicit choice asks viewers for relative motion.
    server.set_relative_pointer(mouse == MouseMode::Relative);
    report(ServerNotice::Serving {
        address: server.local_addr()?,
        display: capture.name.clone(),
        width: capture.width,
        height: capture.height,
    });
    if !input_allowed {
        report(ServerNotice::ViewOnly(true));
        report(ServerNotice::Message(format!(
            "Allow {} in {ACCESSIBILITY_SETTINGS}; remote input starts as soon as access is \
             granted, without a restart.",
            permission_holder()
        )));
    }
    let placement = Mutex::new(InputPlacement {
        display: capture.display,
        width: capture.width,
        height: capture.height,
    });
    // Viewer clipboard text waiting to be written by the serving loop, which
    // owns the pasteboard; a newer text replaces an unwritten one.
    let remote_clipboard = Mutex::new(None);
    thread::scope(|scope| {
        let stop_on_error = |error| {
            shutdown.store(true, Ordering::Release);
            server.stop();
            error
        };
        let listener_thread = thread::Builder::new()
            .name("topvnc-rfb-listener".into())
            .spawn_scoped(scope, || {
                interactive_qos();
                if let Err(error) = server.run() {
                    report(ServerNotice::Message(format!(
                        "VNC listener stopped: {error}"
                    )));
                }
            })
            .map_err(stop_on_error)?;
        // Remote input is injected on its own thread so it never waits for a
        // frame to be copied.
        let input_thread = thread::Builder::new()
            .name("topvnc-input".into())
            .spawn_scoped(scope, || {
                interactive_qos();
                run_input(
                    &server,
                    &placement,
                    &remote_clipboard,
                    input_allowed,
                    shutdown,
                    report,
                )
            })
            .map_err(stop_on_error)?;

        let result = serve_desktop(
            &server,
            display,
            scale,
            capture,
            framebuffer,
            &placement,
            &remote_clipboard,
            shutdown,
            report,
        );
        // Always stop viewers and drain input so no remote key or button stays
        // held, even when capture failed unrecoverably.
        shutdown.store(true, Ordering::Release);
        server.stop();
        listener_thread
            .join()
            .map_err(|_| "VNC listener thread panicked")?;
        input_thread.join().map_err(|_| "input thread panicked")?;
        result
    })
}

/// Run the calling thread at user-interactive QoS, which keeps capture,
/// encoding, and input injection on performance cores while a game or other
/// foreground work loads the efficiency cores.
fn interactive_qos() {
    // SAFETY: changes only the calling thread's scheduling class.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

fn bind_error(error: std::io::Error, address: &str) -> Box<dyn Error> {
    if error.kind() == std::io::ErrorKind::AddrInUse {
        format!(
            "could not listen on {address}: the address is already in use. macOS Screen Sharing \
             or Remote Management may already be listening on port 5900; choose another port, \
             such as 5901"
        )
        .into()
    } else {
        format!("could not listen on {address}: {error}").into()
    }
}

/// Capture the display and sync the pasteboard until shutdown is requested.
#[allow(clippy::too_many_arguments)]
fn serve_desktop(
    server: &VncServer,
    display: Option<usize>,
    scale: f32,
    capture: DisplayCapture,
    mut framebuffer: Framebuffer,
    placement: &Mutex<InputPlacement>,
    remote_clipboard: &Mutex<Option<Vec<u8>>>,
    shutdown: &AtomicBool,
    report: &(dyn Fn(ServerNotice) + Sync),
) -> Result<(), Box<dyn Error>> {
    let mut pasteboard = Pasteboard::new();
    match pasteboard.read_latin1() {
        Ok(text) => server.set_clipboard_text(&text)?,
        Err(error) => report(ServerNotice::Message(format!(
            "Could not read the initial pasteboard: {error}"
        ))),
    }
    let mut last_pasteboard_poll = Instant::now();
    // `None` while capture is paused; it is recreated from the same display
    // selection.
    let mut capture = Some(capture);
    let mut retry_delay = CAPTURE_RETRY_MIN;
    let mut next_retry = Instant::now();
    let mut last_capture_error = String::new();
    let mut last_display_check = Instant::now();
    let mut connections = 0;
    let mut regions = Vec::new();
    while !shutdown.load(Ordering::Acquire) {
        let current_connections = server.active_connections();
        if current_connections != connections {
            connections = current_connections;
            report(ServerNotice::Connections(connections));
        }

        let remote_text = remote_clipboard
            .lock()
            .ok()
            .and_then(|mut text| text.take());
        if let Some(text) = remote_text
            && let Err(error) = pasteboard.write(&text)
        {
            report(ServerNotice::Message(format!(
                "Could not update the pasteboard: {error}"
            )));
        }
        if last_pasteboard_poll.elapsed() >= PASTEBOARD_POLL_INTERVAL {
            last_pasteboard_poll = Instant::now();
            if pasteboard.changed_locally() {
                match pasteboard.read_latin1() {
                    Ok(text) => {
                        if let Err(error) = server.set_clipboard_text(&text) {
                            report(ServerNotice::Message(format!(
                                "Could not publish local clipboard text: {error}"
                            )));
                        }
                    }
                    Err(error) => report(ServerNotice::Message(format!(
                        "Could not read the pasteboard: {error}"
                    ))),
                }
            }
        }

        // A changed pixel size, from a resolution or scale change or a
        // reconnect, needs a stream at the new size.
        if let Some(active) = &capture
            && (DISPLAY_RECONFIGURED.swap(false, Ordering::AcqRel)
                || last_display_check.elapsed() >= DISPLAY_CHECK_INTERVAL)
        {
            last_display_check = Instant::now();
            if let Some(reason) = active.needs_restart(display) {
                report(ServerNotice::Message(format!(
                    "Display changed ({reason}); restarting capture."
                )));
                capture = None;
                next_retry = Instant::now();
                continue;
            }
        }

        let Some(active) = capture.as_ref() else {
            if Instant::now() < next_retry {
                thread::sleep(FRAME_WAIT);
                continue;
            }
            DISPLAY_RECONFIGURED.store(false, Ordering::Release);
            match DisplayCapture::start(display, scale) {
                Ok(recreated) => {
                    let size = (recreated.width, recreated.height);
                    if size != (framebuffer.width() as u16, framebuffer.height() as u16) {
                        report(ServerNotice::Resized {
                            width: size.0,
                            height: size.1,
                        });
                        framebuffer = Framebuffer::new(size.0, size.1)?;
                    }
                    *placement
                        .lock()
                        .map_err(|_| "display placement lock poisoned")? = InputPlacement {
                        display: recreated.display,
                        width: size.0,
                        height: size.1,
                    };
                    report(ServerNotice::Message("Desktop capture resumed.".into()));
                    capture = Some(recreated);
                    retry_delay = CAPTURE_RETRY_MIN;
                    last_capture_error.clear();
                    last_display_check = Instant::now();
                }
                Err(error) => {
                    let message = error.to_string();
                    if message != last_capture_error {
                        report(ServerNotice::Message(format!(
                            "Desktop capture unavailable, retrying: {message}"
                        )));
                        last_capture_error = message;
                    }
                    next_retry = Instant::now() + retry_delay;
                    retry_delay = (retry_delay * 2).min(CAPTURE_RETRY_MAX);
                }
            }
            continue;
        };
        if let Some(error) = active.stopped_error() {
            // Display disconnects, revoked permission, and interrupted capture
            // all stop the stream; keep serving the last image and recreate it.
            report(ServerNotice::Message(format!(
                "Desktop capture paused: {error}"
            )));
            last_capture_error = error;
            capture = None;
            next_retry = Instant::now() + retry_delay;
            continue;
        }
        let Some((buffer, damage)) = active.shared.frames.take_timeout(FRAME_WAIT) else {
            continue;
        };
        match copy_frame(&buffer, &damage, &mut framebuffer) {
            Ok(rects) => {
                regions.clear();
                regions.extend(rects.iter().map(damage_rect));
                if !regions.is_empty() {
                    server.update_framebuffer_regions(&framebuffer, &regions)?;
                }
            }
            Err(error) => {
                // The skipped frame's damage is lost, and later frames carry
                // only their own; copy the next one in full.
                active.request_full_frame();
                match error {
                    // The stream still delivers the old size; check the display now.
                    FrameError::SizeMismatch => DISPLAY_RECONFIGURED.store(true, Ordering::Release),
                    FrameError::Invalid(error) => report(ServerNotice::Message(format!(
                        "Skipped a desktop frame: {error}"
                    ))),
                }
            }
        }
    }
    Ok(())
}

fn damage_rect(rect: &Rect) -> DamageRect {
    DamageRect {
        x: rect.left as u16,
        y: rect.top as u16,
        width: (rect.right - rect.left) as u16,
        height: (rect.bottom - rect.top) as u16,
    }
}

#[derive(Debug)]
enum FrameError {
    /// The frame is not the framebuffer's size.
    SizeMismatch,
    Invalid(&'static str),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizeMismatch => formatter.write_str("frame size does not match the display"),
            Self::Invalid(error) => formatter.write_str(error),
        }
    }
}

impl Error for FrameError {}

/// Copy the damaged regions of a captured frame into `framebuffer` and
/// return them.
fn copy_frame(
    buffer: &PixelBuffer,
    damage: &FrameDamage,
    framebuffer: &mut Framebuffer,
) -> Result<Vec<Rect>, FrameError> {
    let surface = CVPixelBufferGetIOSurface(Some(&buffer.0))
        .ok_or(FrameError::Invalid("frame has no IOSurface"))?;
    let (width, height) = (framebuffer.width(), framebuffer.height());
    if (surface.width(), surface.height()) != (width, height) {
        return Err(FrameError::SizeMismatch);
    }
    if surface.pixel_format() != kCVPixelFormatType_32BGRA {
        return Err(FrameError::Invalid("frame is not 32-bit BGRA"));
    }
    if unsafe { surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) } != 0 {
        return Err(FrameError::Invalid("could not lock the frame surface"));
    }
    struct Unlock<'a>(&'a objc2_io_surface::IOSurfaceRef);
    impl Drop for Unlock<'_> {
        fn drop(&mut self) {
            unsafe {
                self.0
                    .unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut())
            };
        }
    }
    let _unlock = Unlock(&surface);
    let row_pitch = surface.bytes_per_row();
    let length = row_pitch
        .checked_mul(height - 1)
        .and_then(|rows| rows.checked_add(width * 4))
        .ok_or(FrameError::Invalid("frame size overflow"))?;
    if row_pitch < width * 4 || surface.alloc_size() < length {
        return Err(FrameError::Invalid("frame has an invalid row layout"));
    }
    // SAFETY: the locked surface holds `height` rows of `row_pitch` bytes and
    // its allocation covers `length` bytes; it stays locked while borrowed.
    let bytes =
        unsafe { std::slice::from_raw_parts(surface.base_address().as_ptr().cast::<u8>(), length) };
    let surface = CaptureSurface {
        bytes,
        row_pitch,
        width,
        height,
        rotation: Rotation::Identity,
    };
    let rects = damage.rects(Rect::new(0, 0, width as i32, height as i32));
    for rect in &rects {
        surface.copy_rect(*rect, framebuffer.pixels_mut(), width);
    }
    Ok(rects)
}

/// A captured frame. ScreenCaptureKit reuses the IOSurface only after the
/// pixel buffer is released, so holding it keeps the pixels stable.
struct PixelBuffer(CFRetained<CVPixelBuffer>);

// SAFETY: CoreVideo pixel buffers are reference counted thread-safely and the
// pixels are only read while their IOSurface is locked.
unsafe impl Send for PixelBuffer {}

/// State shared between ScreenCaptureKit callbacks and the serving loop.
struct StreamShared {
    frames: FrameSlot<PixelBuffer>,
    /// Why the stream stopped, once it has.
    stopped: Mutex<Option<String>>,
    /// The next complete frame must be copied in full.
    full_frame_pending: AtomicBool,
    /// Treat every frame as fully damaged.
    every_frame_full: bool,
}

struct ObserverIvars {
    shared: Arc<StreamShared>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and the class does
    // not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "TopVNCStreamObserver"]
    #[ivars = ObserverIvars]
    struct StreamObserver;

    unsafe impl NSObjectProtocol for StreamObserver {}

    unsafe impl SCStreamOutput for StreamObserver {
        /// Runs on the capture queue. Only takes the frame hand-off lock,
        /// never the RFB server's.
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output_sample_buffer(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Screen {
                accept_frame(&self.ivars().shared, sample_buffer);
            }
        }
    }

    unsafe impl SCStreamDelegate for StreamObserver {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop_with_error(&self, _stream: &SCStream, error: &NSError) {
            if let Ok(mut stopped) = self.ivars().shared.stopped.lock() {
                stopped.get_or_insert_with(|| error.localizedDescription().to_string());
            }
        }
    }
);

impl StreamObserver {
    fn new(shared: Arc<StreamShared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ObserverIvars { shared });
        unsafe { msg_send![super(this), init] }
    }
}

/// Hand a complete frame and its damage to the serving loop.
fn accept_frame(shared: &StreamShared, sample: &CMSampleBuffer) {
    if !unsafe { sample.is_valid() } {
        return;
    }
    let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else {
        return;
    };
    // SAFETY: CFArray and NSArray are toll-free bridged; ScreenCaptureKit
    // attaches one dictionary of frame info per sample.
    let attachments: &NSArray<NSDictionary<NSString, AnyObject>> =
        unsafe { &*(&*attachments as *const _ as *const NSArray<_>) };
    let Some(info) = attachments.firstObject() else {
        return;
    };
    let status = info
        .objectForKey(unsafe { SCStreamFrameInfoStatus })
        .and_then(|status| {
            status
                .downcast_ref::<NSNumber>()
                .map(NSNumber::integerValue)
        });
    // Idle, blank, and suspended frames carry no new desktop pixels.
    if status != Some(SCFrameStatus::Complete.0) {
        return;
    }
    let Some(buffer) = (unsafe { sample.image_buffer() }) else {
        return;
    };
    let Some(surface) = CVPixelBufferGetIOSurface(Some(&buffer)) else {
        return;
    };
    let damage =
        if shared.full_frame_pending.swap(false, Ordering::AcqRel) || shared.every_frame_full {
            FrameDamage::Full
        } else {
            let dirty = info
                .objectForKey(unsafe { SCStreamFrameInfoDirtyRects })
                .and_then(|rects| rects.downcast::<NSArray>().ok())
                .map(|rects| dirty_rects(&rects));
            frame_damage(dirty.as_deref(), surface.width(), surface.height())
        };
    // The replaced frame, if any, is released after the hand-off lock.
    drop(shared.frames.publish(PixelBuffer(buffer), damage));
}

/// Decode ScreenCaptureKit dirty rectangles, CGRect dictionaries in frame
/// pixels; unreadable entries are dropped.
fn dirty_rects(rects: &NSArray) -> Vec<[f64; 4]> {
    rects
        .iter()
        .filter_map(|rect| {
            let dictionary = rect.downcast_ref::<NSDictionary>()?;
            // SAFETY: NSDictionary and CFDictionary are toll-free bridged.
            let dictionary: &CFDictionary =
                unsafe { &*(dictionary as *const NSDictionary as *const CFDictionary) };
            let mut rect = CGRect::default();
            if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(dictionary), &mut rect) } {
                return None;
            }
            Some([
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height,
            ])
        })
        .collect()
}

/// Wraps a value created on one thread and moved to another exactly once.
struct SendOnce<T>(T);

// SAFETY: only used for retained Objective-C objects that are handed from a
// completion handler to the waiting thread and not used concurrently.
unsafe impl<T> Send for SendOnce<T> {}

fn error_text(error: *mut NSError) -> Option<String> {
    unsafe { error.as_ref() }.map(|error| error.localizedDescription().to_string())
}

/// Run an asynchronous ScreenCaptureKit call and wait for its completion.
fn wait_for_completion(
    call: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
) -> Result<(), String> {
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        let _ = sender.send(error_text(error));
    });
    call(&block);
    match receiver.recv_timeout(COMPLETION_TIMEOUT) {
        Ok(None) => Ok(()),
        Ok(Some(error)) => Err(error),
        Err(_) => Err("ScreenCaptureKit did not respond".into()),
    }
}

/// Displays in `SCShareableContent` order.
fn shareable_displays() -> Result<Vec<Retained<SCDisplay>>, String> {
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = match unsafe { Retained::retain(content) } {
                Some(content) => Ok(SendOnce(content)),
                None => Err(error_text(error).unwrap_or_else(|| "no shareable content".into())),
            };
            let _ = sender.send(result);
        },
    );
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    let content = receiver
        .recv_timeout(COMPLETION_TIMEOUT)
        .map_err(|_| "ScreenCaptureKit did not list displays".to_string())?
        .map_err(|error| format!("could not list displays: {error}"))?
        .0;
    Ok(unsafe { content.displays() }.to_vec())
}

/// The display mode's size in pixels, which on Retina displays is larger
/// than its size in points.
fn display_pixel_size(display: CGDirectDisplayID) -> Result<(u16, u16), String> {
    let mode = CGDisplayCopyDisplayMode(display).ok_or("the display is not active")?;
    let width = CGDisplayMode::pixel_width(Some(&mode));
    let height = CGDisplayMode::pixel_height(Some(&mode));
    let width = u16::try_from(width).map_err(|_| "display width is outside VNC limits")?;
    let height = u16::try_from(height).map_err(|_| "display height is outside VNC limits")?;
    validate_capture_dimensions(width, height)?;
    Ok((width, height))
}

/// The display's localized name, read on the main thread as AppKit requires.
/// Gives up when the main thread does not run its queue promptly.
fn display_name(display: CGDirectDisplayID) -> Option<String> {
    let (sender, receiver) = mpsc::channel();
    DispatchQueue::main().exec_async(move || {
        let name = MainThreadMarker::new().and_then(|mtm| {
            NSScreen::screens(mtm)
                .iter()
                .find(|screen| {
                    screen
                        .deviceDescription()
                        .objectForKey(ns_string!("NSScreenNumber"))
                        .and_then(|number| {
                            number
                                .downcast_ref::<NSNumber>()
                                .map(NSNumber::unsignedIntValue)
                        })
                        == Some(display)
                })
                .map(|screen| screen.localizedName().to_string())
        });
        let _ = sender.send(name);
    });
    receiver.recv_timeout(DISPLAY_NAME_TIMEOUT).ok().flatten()
}

/// A running ScreenCaptureKit stream of one display, at its pixel size or
/// scaled down by ScreenCaptureKit.
struct DisplayCapture {
    stream: Retained<SCStream>,
    observer: Retained<StreamObserver>,
    shared: Arc<StreamShared>,
    _queue: DispatchRetained<DispatchQueue>,
    display: CGDirectDisplayID,
    /// The display's pixel size, which a restart is needed to follow.
    native: (u16, u16),
    /// The served size of each frame.
    width: u16,
    height: u16,
    name: String,
}

impl DisplayCapture {
    /// Capture the display numbered `display` (1-based, in `SCShareableContent`
    /// order), or the main display, at `scale` times its pixel size.
    fn start(display: Option<usize>, scale: f32) -> Result<Self, Box<dyn Error>> {
        let displays = shareable_displays()?;
        if displays.is_empty() {
            // ScreenCaptureKit lists no displays while the session is locked.
            return Err(
                "no display is available to capture; the Mac may be locked or its display asleep"
                    .into(),
            );
        }
        let count = displays.len();
        let selected = match display {
            Some(number) => displays.get(number - 1).ok_or_else(|| {
                format!("display {number} not found; {count} display(s) are available")
            })?,
            None => {
                let main = CGMainDisplayID();
                displays
                    .iter()
                    .find(|display| unsafe { display.displayID() } == main)
                    .or(displays.first())
                    .ok_or("no display is available to capture")?
            }
        };
        let id = unsafe { selected.displayID() };
        let native = display_pixel_size(id)?;
        let (width, height) = served_size(native.0, native.1, scale);
        // A 120 Hz display captured at 60 fps adds up to a frame of latency.
        let rate = capture_rate(
            CGDisplayCopyDisplayMode(id)
                .map_or(0.0, |mode| CGDisplayMode::refresh_rate(Some(&mode))),
        );

        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                selected,
                &NSArray::new(),
            )
        };
        let configuration = unsafe { SCStreamConfiguration::new() };
        unsafe {
            configuration.setWidth(usize::from(width));
            configuration.setHeight(usize::from(height));
            configuration.setMinimumFrameInterval(CMTime::new(1, rate));
            configuration.setPixelFormat(kCVPixelFormatType_32BGRA);
            configuration.setQueueDepth(STREAM_QUEUE_DEPTH);
            configuration.setShowsCursor(true);
            configuration.setCapturesAudio(false);
        }
        let shared = Arc::new(StreamShared {
            frames: FrameSlot::default(),
            stopped: Mutex::new(None),
            full_frame_pending: AtomicBool::new(true),
            // ScreenCaptureKit scales on the GPU. Its dirty rectangles are not
            // relied on for scaled frames: each frame is copied whole and the
            // RFB server's tile comparison finds what changed.
            every_frame_full: (width, height) != native,
        });
        let observer = StreamObserver::new(Arc::clone(&shared));
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &configuration,
                Some(ProtocolObject::from_ref(&*observer)),
            )
        };
        // Frames are handed off on this queue; keep it on performance cores.
        let queue = DispatchQueue::new(
            "com.topvnc.capture",
            Some(&DispatchQueueAttr::with_qos_class(
                DispatchQueueAttr::SERIAL,
                DispatchQoS::UserInteractive,
                0,
            )),
        );
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*observer),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|error| {
            format!(
                "could not receive display frames: {}",
                error.localizedDescription()
            )
        })?;
        wait_for_completion(|handler| unsafe {
            stream.startCaptureWithCompletionHandler(Some(handler))
        })
        .map_err(|error| format!("could not start display capture: {error}"))?;
        Ok(Self {
            stream,
            observer,
            shared,
            _queue: queue,
            display: id,
            native,
            width,
            height,
            name: display_name(id).unwrap_or_else(|| format!("display {id}")),
        })
    }

    fn stopped_error(&self) -> Option<String> {
        self.shared.stopped.lock().ok()?.clone()
    }

    /// Treat the next complete frame as fully damaged.
    fn request_full_frame(&self) {
        self.shared
            .full_frame_pending
            .store(true, Ordering::Release);
    }

    /// Why the stream no longer matches the display it should capture.
    fn needs_restart(&self, display: Option<usize>) -> Option<String> {
        if display.is_none() && CGMainDisplayID() != self.display {
            return Some("the main display changed".into());
        }
        match display_pixel_size(self.display) {
            Ok(size) if size == self.native => None,
            Ok((width, height)) => Some(format!("now {width}x{height} pixels")),
            Err(error) => Some(error),
        }
    }
}

impl Drop for DisplayCapture {
    fn drop(&mut self) {
        if self.stopped_error().is_none() {
            let _ = wait_for_completion(|handler| unsafe {
                self.stream.stopCaptureWithCompletionHandler(Some(handler))
            });
        }
        let _ = unsafe {
            self.stream.removeStreamOutput_type_error(
                ProtocolObject::from_ref(&*self.observer),
                SCStreamOutputType::Screen,
            )
        };
    }
}

unsafe extern "C-unwind" fn display_reconfigured(
    _display: CGDirectDisplayID,
    flags: CGDisplayChangeSummaryFlags,
    _user_info: *mut c_void,
) {
    if !flags.contains(CGDisplayChangeSummaryFlags::BeginConfigurationFlag) {
        DISPLAY_RECONFIGURED.store(true, Ordering::Release);
    }
}

/// Registers the display reconfiguration callback while serving.
struct ReconfigurationCallback;

impl ReconfigurationCallback {
    fn register() -> Self {
        DISPLAY_RECONFIGURED.store(false, Ordering::Release);
        unsafe {
            CGDisplayRegisterReconfigurationCallback(
                Some(display_reconfigured),
                std::ptr::null_mut(),
            );
        }
        Self
    }
}

impl Drop for ReconfigurationCallback {
    fn drop(&mut self) {
        unsafe {
            CGDisplayRemoveReconfigurationCallback(
                Some(display_reconfigured),
                std::ptr::null_mut(),
            );
        }
    }
}

/// The general pasteboard, tracking which changes this host made itself.
struct Pasteboard {
    pasteboard: Retained<NSPasteboard>,
    change_count: isize,
}

impl Pasteboard {
    fn new() -> Self {
        let pasteboard = NSPasteboard::generalPasteboard();
        let change_count = pasteboard.changeCount();
        Self {
            pasteboard,
            change_count,
        }
    }

    /// Whether another app changed the pasteboard since it was last seen.
    fn changed_locally(&mut self) -> bool {
        let current = self.pasteboard.changeCount();
        let changed = current != self.change_count;
        self.change_count = current;
        changed
    }

    fn read_latin1(&self) -> Result<Vec<u8>, &'static str> {
        let Some(text) = self
            .pasteboard
            .stringForType(unsafe { NSPasteboardTypeString })
        else {
            return Ok(Vec::new());
        };
        // UTF-16 length bounds the character count.
        if text.length() > 2 * MAX_CLIPBOARD_CHARS {
            return Err("pasteboard text exceeds clipboard limits");
        }
        Ok(latin1_from_unicode(&text.to_string()))
    }

    /// Write viewer text, recording the change so it is not echoed back.
    fn write(&mut self, text: &[u8]) -> Result<(), &'static str> {
        let text = NSString::from_str(&latin1_to_string(text));
        self.pasteboard.clearContents();
        let written = self
            .pasteboard
            .setString_forType(&text, unsafe { NSPasteboardTypeString });
        self.change_count = self.pasteboard.changeCount();
        if written {
            Ok(())
        } else {
            Err("the pasteboard rejected the text")
        }
    }
}

fn run_input(
    server: &VncServer,
    placement: &Mutex<InputPlacement>,
    remote_clipboard: &Mutex<Option<Vec<u8>>>,
    input_allowed: bool,
    shutdown: &AtomicBool,
    report: &(dyn Fn(ServerNotice) + Sync),
) {
    // Events are still drained without an injector so viewers never stall
    // on a full event queue.
    let mut injector = Injector::new();
    if injector.is_none() {
        report(ServerNotice::Message(
            "Could not create an input event source; remote input is unavailable.".into(),
        ));
    }
    let mut input_allowed = input_allowed;
    let mut last_access_check = Instant::now();
    while !shutdown.load(Ordering::Acquire) {
        if !input_allowed && last_access_check.elapsed() >= ACCESSIBILITY_POLL_INTERVAL {
            last_access_check = Instant::now();
            input_allowed = unsafe { AXIsProcessTrusted() } != 0;
            if input_allowed {
                report(ServerNotice::ViewOnly(false));
            }
        }
        let event = match server.recv_event_timeout(INPUT_POLL_INTERVAL) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match (event, injector.as_mut()) {
            // Without Accessibility access, macOS drops posted events; ignore
            // them so no key or button is recorded as held.
            (
                ClientEvent::Key { .. }
                | ClientEvent::Pointer { .. }
                | ClientEvent::RelativePointer { .. },
                _,
            ) if !input_allowed => {}
            (
                ClientEvent::Key {
                    client_id,
                    keysym,
                    down,
                },
                Some(injector),
            ) => injector.key(client_id, keysym, down),
            (
                ClientEvent::Pointer {
                    client_id,
                    buttons,
                    x,
                    y,
                },
                Some(injector),
            ) => {
                let placement = placement.lock().ok().map(|placement| *placement);
                if let Some(placement) = placement {
                    injector.pointer(client_id, buttons, x, y, placement);
                }
            }
            (
                ClientEvent::RelativePointer {
                    client_id,
                    buttons,
                    dx,
                    dy,
                },
                Some(injector),
            ) => {
                let placement = placement.lock().ok().map(|placement| *placement);
                if let Some(placement) = placement {
                    injector.relative_pointer(client_id, buttons, dx, dy, placement);
                }
            }
            (ClientEvent::ClientDisconnected { client_id }, Some(injector)) => {
                injector.disconnect(client_id)
            }
            (ClientEvent::ClipboardText { text, .. }, _) => {
                if let Ok(mut pending) = remote_clipboard.lock() {
                    *pending = Some(text.clone());
                }
                if let Err(error) = server.set_clipboard_text(&text) {
                    report(ServerNotice::Message(format!(
                        "Could not notify other VNC clients about clipboard text: {error}"
                    )));
                }
            }
            (_, None) => {}
        }
    }
}

/// Posts remote input as Quartz events. Dropping it releases every remotely
/// held key and button.
struct Injector {
    source: CFRetained<CGEventSource>,
    input: RemoteInputState<KeyIdentity<MacKeyCode>>,
    clicks: ClickTracker,
    double_click_interval: Duration,
    last_point: Option<CGPoint>,
}

const BUTTONS: [(u16, CGEventType, CGEventType, CGMouseButton); 5] = [
    (
        BUTTON_LEFT,
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGMouseButton::Left,
    ),
    (
        BUTTON_MIDDLE,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGMouseButton::Center,
    ),
    (
        BUTTON_RIGHT,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGMouseButton::Right,
    ),
    // Other-mouse buttons 3 and 4 are back and forward.
    (
        BUTTON_BACK,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGMouseButton(3),
    ),
    (
        BUTTON_FORWARD,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGMouseButton(4),
    ),
];

impl Injector {
    fn new() -> Option<Self> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)?;
        // By default, local hardware input is suppressed briefly after each
        // posted event, which would fight the person at the Mac.
        CGEventSource::set_local_events_suppression_interval(Some(&source), 0.0);
        Some(Self {
            source,
            input: RemoteInputState::default(),
            clicks: ClickTracker::default(),
            double_click_interval: Duration::try_from_secs_f64(NSEvent::doubleClickInterval())
                .unwrap_or(Duration::from_millis(500)),
            last_point: None,
        })
    }

    /// Modifier flags of every remotely held modifier, plus the system's
    /// Caps Lock state, set explicitly so synthetic modifier tracking never
    /// decides them.
    fn modifier_flags(&self) -> u64 {
        let caps_lock = CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState).0
            & MAC_FLAG_ALPHA_SHIFT;
        macos_event_flags(self.input.held_keys()) | caps_lock
    }

    fn post(&self, event: &CGEvent, flags: u64) {
        CGEvent::set_flags(Some(event), CGEventFlags(flags));
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
    }

    fn key(&mut self, client_id: u64, keysym: u32, down: bool) {
        let key = macos_key_identity(keysym);
        let repeat = down && self.input.is_held_by(client_id, &key);
        if self.input.key_event(client_id, key, down) {
            self.post_key(key, down, repeat);
        }
    }

    fn post_key(&self, key: KeyIdentity<MacKeyCode>, down: bool, repeat: bool) {
        let flags = self.modifier_flags();
        match key {
            KeyIdentity::Virtual(code) => {
                let Some(event) = CGEvent::new_keyboard_event(Some(&self.source), code, down)
                else {
                    return;
                };
                if macos_modifier_flags(code).is_some() {
                    // Modifier keys produce flags-changed events, not key
                    // presses; the flags already reflect this transition.
                    CGEvent::set_type(Some(&event), CGEventType::FlagsChanged);
                    self.post(&event, flags);
                } else {
                    if repeat {
                        CGEvent::set_integer_value_field(
                            Some(&event),
                            CGEventField::KeyboardEventAutorepeat,
                            1,
                        );
                    }
                    self.post(&event, flags | macos_key_flags(code));
                }
            }
            KeyIdentity::Unicode(keysym) => {
                let Some((first, second)) = unicode_key_units(keysym) else {
                    return;
                };
                let units = [first, second.unwrap_or(0)];
                let length = if second.is_some() { 2 } else { 1 };
                let Some(event) = CGEvent::new_keyboard_event(Some(&self.source), 0, down) else {
                    return;
                };
                unsafe {
                    CGEvent::keyboard_set_unicode_string(Some(&event), length, units.as_ptr())
                };
                self.post(&event, flags);
            }
        }
    }

    fn pointer(&mut self, client_id: u64, buttons: u16, x: u16, y: u16, placement: InputPlacement) {
        let transition = self.input.pointer_event(client_id, buttons);
        let point = display_point(
            x,
            y,
            (placement.width, placement.height),
            display_bounds(placement.display),
        )
        .map(|(x, y)| CGPoint { x, y })
        .or(self.last_point)
        .unwrap_or_else(current_pointer_location);
        self.move_and_press(transition, point, None, (i32::from(x), i32::from(y)));
    }

    /// Move the pointer by (`dx`, `dy`) points, kept on the display, and
    /// report the delta to applications that read it, as games do.
    fn relative_pointer(
        &mut self,
        client_id: u64,
        buttons: u16,
        dx: i32,
        dy: i32,
        placement: InputPlacement,
    ) {
        let transition = self.input.pointer_event(client_id, buttons);
        // The system's position, so moves made locally or by the
        // application, such as recentering, are respected.
        let from = current_pointer_location();
        let point = relative_point((from.x, from.y), dx, dy, display_bounds(placement.display))
            .map_or(from, |(x, y)| CGPoint { x, y });
        let delta = (dx != 0 || dy != 0).then_some((dx, dy));
        self.move_and_press(transition, point, delta, (point.x as i32, point.y as i32));
    }

    /// Move to `point`, with `delta` in the event's delta fields when the
    /// motion is relative, then post button and wheel changes there. Click
    /// counts compare `click_at` between presses.
    fn move_and_press(
        &mut self,
        transition: PointerTransition,
        point: CGPoint,
        delta: Option<(i32, i32)>,
        click_at: (i32, i32),
    ) {
        let flags = self.modifier_flags();
        if self.last_point != Some(point) || delta.is_some() {
            self.last_point = Some(point);
            // Moves with a button held are drags of the lowest held button.
            let (kind, button) = match transition.previous {
                held if held & BUTTON_LEFT != 0 => {
                    (CGEventType::LeftMouseDragged, CGMouseButton::Left)
                }
                held if held & BUTTON_RIGHT != 0 => {
                    (CGEventType::RightMouseDragged, CGMouseButton::Right)
                }
                held if held & BUTTON_MIDDLE != 0 => {
                    (CGEventType::OtherMouseDragged, CGMouseButton::Center)
                }
                held if held & BUTTON_BACK != 0 => {
                    (CGEventType::OtherMouseDragged, CGMouseButton(3))
                }
                held if held & BUTTON_FORWARD != 0 => {
                    (CGEventType::OtherMouseDragged, CGMouseButton(4))
                }
                _ => (CGEventType::MouseMoved, CGMouseButton::Left),
            };
            if let Some(event) = CGEvent::new_mouse_event(Some(&self.source), kind, point, button) {
                if let Some((dx, dy)) = delta {
                    for (field, value) in [
                        (CGEventField::MouseEventDeltaX, dx),
                        (CGEventField::MouseEventDeltaY, dy),
                    ] {
                        CGEvent::set_integer_value_field(Some(&event), field, i64::from(value));
                    }
                }
                self.post(&event, flags);
            }
        }
        let now = Instant::now();
        for (mask, down, up, button) in BUTTONS {
            if transition.previous & mask == transition.buttons & mask {
                continue;
            }
            let pressed = transition.buttons & mask != 0;
            let count = if pressed {
                self.clicks.press(
                    mask,
                    click_at.0,
                    click_at.1,
                    now,
                    self.double_click_interval,
                )
            } else {
                self.clicks.release(mask)
            };
            self.post_button(if pressed { down } else { up }, button, point, count, flags);
        }
        if transition.vertical_notches != 0 || transition.horizontal_notches != 0 {
            // Wheel 2 scrolls left for positive values.
            if let Some(event) = CGEvent::new_scroll_wheel_event2(
                Some(&self.source),
                CGScrollEventUnit::Line,
                2,
                transition.vertical_notches,
                -transition.horizontal_notches,
                0,
            ) {
                CGEvent::set_location(Some(&event), point);
                self.post(&event, flags);
            }
        }
    }

    fn post_button(
        &self,
        kind: CGEventType,
        button: CGMouseButton,
        point: CGPoint,
        click_count: i64,
        flags: u64,
    ) {
        if let Some(event) = CGEvent::new_mouse_event(Some(&self.source), kind, point, button) {
            CGEvent::set_integer_value_field(
                Some(&event),
                CGEventField::MouseEventClickState,
                click_count,
            );
            self.post(&event, flags);
        }
    }

    /// Release buttons that went from `previous` to `buttons` where the
    /// pointer is now.
    fn release_buttons(&self, previous: u16, buttons: u16) {
        if previous & !buttons == 0 {
            return;
        }
        let point = current_pointer_location();
        let flags = self.modifier_flags();
        for (mask, _, up, button) in BUTTONS {
            if previous & mask != 0 && buttons & mask == 0 {
                self.post_button(up, button, point, self.clicks.release(mask), flags);
            }
        }
    }

    fn disconnect(&mut self, client_id: u64) {
        let (released, previous, buttons) = self.input.disconnect(client_id);
        for key in released {
            self.post_key(key, false, false);
        }
        self.release_buttons(previous, buttons);
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        let (keys, buttons) = self.input.release_all();
        for key in keys {
            self.post_key(key, false, false);
        }
        self.release_buttons(buttons, 0);
    }
}

/// The display's current placement in global points, so moves follow
/// arrangement and scale changes as soon as they happen.
fn display_bounds(display: CGDirectDisplayID) -> DisplayBounds {
    let bounds = CGDisplayBounds(display);
    DisplayBounds {
        x: bounds.origin.x,
        y: bounds.origin.y,
        width: bounds.size.width,
        height: bounds.size.height,
    }
}

fn current_pointer_location() -> CGPoint {
    CGEvent::new(None)
        .map(|event| CGEvent::location(Some(&event)))
        .unwrap_or(CGPoint { x: 0.0, y: 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_request_graceful_server_shutdown() {
        SERVER_SHUTDOWN_REQUESTED.store(false, Ordering::Release);
        request_shutdown(libc::SIGINT);
        assert!(SERVER_SHUTDOWN_REQUESTED.load(Ordering::Acquire));
        SERVER_SHUTDOWN_REQUESTED.store(false, Ordering::Release);
    }

    #[test]
    fn port_conflicts_name_screen_sharing_and_suggest_another_port() {
        let error = bind_error(
            std::io::Error::from(std::io::ErrorKind::AddrInUse),
            "0.0.0.0:5900",
        )
        .to_string();
        assert!(
            error.contains("Screen Sharing or Remote Management"),
            "{error}"
        );
        assert!(error.contains("5901"), "{error}");
        let error = bind_error(
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            "0.0.0.0:80",
        )
        .to_string();
        assert!(!error.contains("Screen Sharing"), "{error}");
    }

    #[test]
    fn this_mac_meets_the_screencapturekit_minimum() {
        assert_eq!(require_supported_macos(), Ok(()));
    }

    #[test]
    fn reconfiguration_callback_ignores_the_begin_phase() {
        DISPLAY_RECONFIGURED.store(false, Ordering::Release);
        unsafe {
            display_reconfigured(
                1,
                CGDisplayChangeSummaryFlags::BeginConfigurationFlag,
                std::ptr::null_mut(),
            );
        }
        assert!(!DISPLAY_RECONFIGURED.load(Ordering::Acquire));
        unsafe {
            display_reconfigured(
                1,
                CGDisplayChangeSummaryFlags::SetModeFlag,
                std::ptr::null_mut(),
            );
        }
        assert!(DISPLAY_RECONFIGURED.swap(false, Ordering::AcqRel));
    }

    #[test]
    fn screencapturekit_frames_decode_dirty_rectangles() {
        let rect = |x: f64, y: f64, width: f64, height: f64| {
            NSDictionary::<NSString, AnyObject>::from_slices(
                &[
                    ns_string!("X"),
                    ns_string!("Y"),
                    ns_string!("Width"),
                    ns_string!("Height"),
                ],
                &[
                    NSNumber::new_f64(x).as_ref(),
                    NSNumber::new_f64(y).as_ref(),
                    NSNumber::new_f64(width).as_ref(),
                    NSNumber::new_f64(height).as_ref(),
                ],
            )
        };
        let first = rect(1.0, 2.0, 30.0, 40.0);
        let second = rect(0.5, 0.0, 1.0, 1.0);
        let not_a_rect = NSString::from_str("nope");
        let rects = NSArray::<AnyObject>::from_slice(&[
            first.as_ref(),
            not_a_rect.as_ref(),
            second.as_ref(),
        ]);
        assert_eq!(
            dirty_rects(&rects),
            [[1.0, 2.0, 30.0, 40.0], [0.5, 0.0, 1.0, 1.0]]
        );
    }
}
