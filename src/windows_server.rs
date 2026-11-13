//! Windows desktop host for `topvnc --serve` and the Server tab: DXGI Desktop
//! Duplication capture, `SendInput` injection, and clipboard sync.

use crate::desktop_host::{
    CAPTURE_RETRY_MAX, CAPTURE_RETRY_MIN, CaptureOutage, CaptureSurface, CursorKind, CursorShape,
    DesktopImage, DisplayArea, DisplayChange, DisplayFollower, DisplayPlan, DisplaySelection,
    Downscaler, KeyIdentity, MAX_DISPLAYS, MouseMode, PointerModeHint, PointerOwner,
    PointerTransition, Rect, RemoteInputState, Rotation, WHEEL_DELTA, absolute_mouse_coordinate,
    connected_displays, is_secure_attention, latin1_from_unicode, latin1_to_utf16,
    native_coordinate, served_size, unicode_key_units, validate_capture_dimensions,
    windows_key_identity,
};
use crate::desktop_host::{
    MAX_CLIPBOARD_CHARS, ServeOptions, ServerNotice, VirtualKey, parse_serve_arguments,
    report_serving,
};
use std::collections::HashSet;
use std::error::Error;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use topvnc::{
    BUTTON_BACK, BUTTON_FORWARD, BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, CaptureStatus,
    ClientEvent, DamageRect, Framebuffer, ServerConfig, VncServer,
};
use windows::Win32::Foundation::{E_ACCESSDENIED, HMODULE, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_IDENTITY,
    DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
    DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_MORE_DATA, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_MOVE_RECT, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME, DXGI_OUTPUT_DESC, IDXGIAdapter1, IDXGIDevice,
    IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::core::{BOOL, Interface};
use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, GENERIC_ALL, GetLastError, GlobalFree, SetLastError,
};
use windows_sys::Win32::Media::{TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, HDESK, OpenInputDesktop,
    SetThreadDesktop, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThreadId, GetPriorityClass, HIGH_PRIORITY_CLASS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
    ProcessPowerThrottling, SetPriorityClass, SetProcessInformation,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    SetProcessDpiAwarenessContext, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MapVirtualKeyW, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSOR_SUPPRESSED, CURSORINFO, CreateWindowExW, DestroyWindow, GetCursorInfo,
    GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetProcessDPIAware, XBUTTON1, XBUTTON2,
};

static SERVER_SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Set in the host agent, which the host service runs as SYSTEM: capture
/// and input then follow the desktop that receives input, including the
/// lock screen and UAC prompts on the secure desktop.
static FOLLOW_INPUT_DESKTOP: AtomicBool = AtomicBool::new(false);
/// Asks the host service to start Ctrl+Alt+Delete; set with
/// [`FOLLOW_INPUT_DESKTOP`].
static SECURE_ATTENTION: OnceLock<fn()> = OnceLock::new();
/// How often the input thread checks which desktop receives input.
const INPUT_DESKTOP_CHECK_INTERVAL: Duration = Duration::from_millis(250);
/// How long one capture poll waits for a new desktop frame.
const FRAME_TIMEOUT_MS: u32 = 50;
const FRAME_TIMEOUT: Duration = Duration::from_millis(FRAME_TIMEOUT_MS as u64);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(1);
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How often the monitor layout is compared, to follow displays that are
/// plugged in, unplugged, or made primary without interrupting capture.
const LAYOUT_CHECK_INTERVAL: Duration = Duration::from_millis(500);
/// Above this many changed rectangles, copy the whole frame to the CPU once.
const MAX_REGION_COPIES: usize = 64;
/// Captured regions waiting for the serving loop; beyond this they collapse
/// to the whole image.
const MAX_PENDING_RECTS: usize = 256;
/// Monitors GDI may report; more than any desktop attaches.
const MAX_MONITORS: usize = 64;
const MAX_POINTER_SHAPE_BYTES: u32 = 16 * 1024 * 1024;
const MAX_FRAME_METADATA_BYTES: u32 = 16 * 1024 * 1024;

unsafe extern "system" fn console_control_handler(control: u32) -> i32 {
    if matches!(
        control,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    ) {
        SERVER_SHUTDOWN_REQUESTED.store(true, Ordering::Release);
        1
    } else {
        0
    }
}

/// Where the served display sits on the Windows virtual desktop, in physical
/// pixels. Shared with the input thread to place the pointer.
#[derive(Debug, Clone, Copy)]
struct DisplayPlacement {
    left: i32,
    top: i32,
    width: u16,
    height: u16,
    /// The framebuffer size viewers see, smaller than the display when the
    /// served image is scaled down.
    served_width: u16,
    served_height: u16,
}

/// What viewers see: the captured image itself, or a downscaled copy.
struct ServedImage {
    scaled: Option<(Downscaler, Framebuffer)>,
    regions: Vec<DamageRect>,
}

impl ServedImage {
    fn new(native: (u16, u16), scale: f32) -> Result<Self, Box<dyn Error>> {
        let served = served_size(native.0, native.1, scale);
        let scaled = if served == native {
            None
        } else {
            Some((
                Downscaler::new(native, served),
                Framebuffer::new(served.0, served.1)?,
            ))
        };
        Ok(Self {
            scaled,
            regions: Vec::new(),
        })
    }

    fn size(&self, captured: &Framebuffer) -> (u16, u16) {
        let image = self.image(captured);
        (image.width() as u16, image.height() as u16)
    }

    fn image<'a>(&'a self, captured: &'a Framebuffer) -> &'a Framebuffer {
        self.scaled
            .as_ref()
            .map_or(captured, |(_, framebuffer)| framebuffer)
    }

    /// Bring the served image up to date with `damage`, in captured pixels.
    /// `regions` then holds the served regions that changed.
    fn update(&mut self, captured: &Framebuffer, damage: &[Rect]) {
        self.regions.clear();
        match &mut self.scaled {
            None => self.regions.extend(damage.iter().map(damage_rect)),
            Some((scaler, served)) => {
                for rect in damage {
                    if let Some(area) = scaler.served_rect(*rect) {
                        scaler.scale(captured.pixels(), served.pixels_mut(), area);
                        self.regions.push(damage_rect(&area));
                    }
                }
            }
        }
    }
}

/// Run `topvnc --serve` until the console asks the server to stop.
pub fn run(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let options = parse_serve_arguments(arguments)?;
    options.tls.server()?;
    // Capture sizes, cursor positions, and injected pointer coordinates must
    // all be physical pixels, even on scaled displays.
    enable_dpi_awareness();
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
    if unsafe { SetConsoleCtrlHandler(Some(console_control_handler), 1) } == 0 {
        return Err("could not register the server shutdown handler".into());
    }
    serve(
        options,
        password,
        &SERVER_SHUTDOWN_REQUESTED,
        &|notice| match notice {
            // The CLI has no status view; connection counts would only be noise.
            ServerNotice::Connections(_) => {}
            notice => eprintln!("{notice}"),
        },
    )
}

/// Capture and serve a display until `shutdown` is set. Runs on the calling
/// thread; `report` receives progress from the capture and input threads.
pub fn serve(
    options: ServeOptions,
    password: Option<String>,
    shutdown: &AtomicBool,
    report: &(dyn Fn(ServerNotice) + Sync),
) -> Result<(), Box<dyn Error>> {
    let tls = options.tls.server()?;
    report(ServerNotice::Message(if tls.is_some() {
        "TLS 1.3 encrypted transport enabled.".into()
    } else {
        "WARNING: explicit unencrypted TCP transport.".into()
    }));
    let ServeOptions {
        tls: _,
        address,
        display,
        allow_insecure,
        scale,
        mouse,
        priority,
        performance,
    } = options;
    // The GUI process stays DPI-unaware for its own windows, so the capture
    // and input threads opt in to physical pixels individually.
    enable_thread_dpi_awareness();
    // The game runs in the foreground; keep capture and encoding on pace.
    let _scheduling = HostScheduling::raise(report);
    if let Err(error) = follow_input_desktop() {
        report(ServerNotice::Message(error));
    }
    let mut follower = DisplayFollower::new(display);
    let (capture, plan) = start_capture(&mut follower).map_err(|error| {
        format!(
            "{error}. Run the server in an interactive, unlocked desktop session; \
             services, disconnected Remote Desktop sessions, and the secure desktop cannot be captured"
        )
    })?;
    if !capture.gpu_priority_raised {
        report(ServerNotice::Message(
            "Windows did not raise the capture GPU priority; capture may wait behind a busy game."
                .into(),
        ));
    }
    let mut framebuffer = Framebuffer::new(capture.placement.width, capture.placement.height)?;
    let mut damage = Vec::new();
    capture.next_frame(framebuffer.pixels_mut(), &mut damage)?;
    let native = (capture.placement.width, capture.placement.height);
    let mut served = ServedImage::new(native, scale)?;
    served.update(
        &framebuffer,
        &[Rect::new(0, 0, i32::from(native.0), i32::from(native.1))],
    );
    let (served_width, served_height) = served.size(&framebuffer);
    let config = ServerConfig {
        tls,
        priority,
        performance,
        name: "TopVNC Windows Desktop".into(),
        password,
        allow_insecure,
        // The high priority class already covers every thread.
        thread_setup: None,
    };
    let server = VncServer::bind(&address, served.image(&framebuffer).clone(), config)?;
    server.set_serve_scale((scale * 100.0).round() as u8);
    server.set_serve_display(plan.announcement());
    report_serving(
        &server,
        report,
        plan.description(),
        served_width,
        served_height,
    )?;
    if let Some(notice) = plan.fallback_notice() {
        report(ServerNotice::Message(notice));
    }
    let placement = Mutex::new(DisplayPlacement {
        served_width,
        served_height,
        ..capture.placement
    });
    // Remote input is injected on its own thread so it never waits for a
    // frame to be captured or copied.
    let remote_clipboard_sequence = AtomicU32::new(0);
    thread::scope(|scope| {
        // A thread that fails to spawn stops the ones already running.
        let stop_on_error = |error| {
            shutdown.store(true, Ordering::Release);
            server.stop();
            error
        };
        let listener_thread = thread::Builder::new()
            .name("topvnc-rfb-listener".into())
            .spawn_scoped(scope, || {
                if let Err(error) = server.run() {
                    report(ServerNotice::Message(format!(
                        "VNC listener stopped: {error}"
                    )));
                }
            })
            .map_err(stop_on_error)?;
        let input_thread = thread::Builder::new()
            .name("topvnc-input".into())
            .spawn_scoped(scope, || {
                enable_thread_dpi_awareness();
                run_input(
                    &server,
                    &placement,
                    &remote_clipboard_sequence,
                    shutdown,
                    report,
                )
            })
            .map_err(stop_on_error)?;

        let result = serve_desktop(
            &server,
            follower,
            scale,
            mouse,
            capture,
            framebuffer,
            served,
            &placement,
            &remote_clipboard_sequence,
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

/// Capture the desktop and sync the clipboard until shutdown is requested.
#[allow(clippy::too_many_arguments)]
fn serve_desktop(
    server: &VncServer,
    mut follower: DisplayFollower,
    mut scale: f32,
    mouse: MouseMode,
    capture: DesktopCapture,
    mut framebuffer: Framebuffer,
    mut served: ServedImage,
    placement: &Mutex<DisplayPlacement>,
    remote_clipboard_sequence: &AtomicU32,
    shutdown: &AtomicBool,
    report: &(dyn Fn(ServerNotice) + Sync),
) -> Result<(), Box<dyn Error>> {
    let mut damage = Vec::new();
    let mut clipboard_sequence = unsafe { GetClipboardSequenceNumber() };
    match read_system_clipboard_latin1() {
        Ok(text) => server.set_clipboard_text(&text)?,
        Err(error) => report(ServerNotice::Message(format!(
            "Could not read the initial system clipboard: {error}"
        ))),
    }
    let mut capture = Some(capture);
    let mut retry_delay = CAPTURE_RETRY_MIN;
    let mut next_retry = Instant::now();
    let mut last_capture_error = String::new();
    let mut last_layout_check = Instant::now();
    let mut connections = 0;
    let mut pointer_mode = PointerModeHint::default();
    let mut outage = CaptureOutage::default();
    while !shutdown.load(Ordering::Acquire) {
        if capture.is_none() {
            outage.stopped(Instant::now());
        }
        // Viewers keep the last picture while capture is stopped; tell them
        // why once it lasts, and that it is live again once it resumes.
        server.set_capture_status(outage.status(Instant::now()));
        server.set_relative_pointer(match mouse {
            // Games that turn the camera with the mouse hide the cursor;
            // ask viewers for relative motion while it stays hidden.
            MouseMode::Auto => pointer_mode.update(cursor_showing(), Instant::now()),
            MouseMode::Relative => true,
            MouseMode::Absolute => false,
        });
        let current_connections = server.active_connections();
        if current_connections != connections {
            connections = current_connections;
            report(ServerNotice::Connections(connections));
        }
        let current_sequence = unsafe { GetClipboardSequenceNumber() };
        if current_sequence != clipboard_sequence {
            clipboard_sequence = current_sequence;
            // Text a viewer just set was already sent to every viewer.
            if current_sequence != remote_clipboard_sequence.load(Ordering::Acquire) {
                match read_system_clipboard_latin1() {
                    Ok(text) => {
                        if let Err(error) = server.set_clipboard_text(&text) {
                            report(ServerNotice::Message(format!(
                                "Could not publish local clipboard text: {error}"
                            )));
                        }
                    }
                    Err(error) => report(ServerNotice::Message(format!(
                        "Could not read the system clipboard: {error}"
                    ))),
                }
            }
        }

        // Viewers share one display choice; the last request wins.
        let mut change = DisplayChange::None;
        if let Some(request) = server.take_display_request() {
            let before = follower.selection();
            change = follower.request(request, attached_areas);
            if follower.selection() != before {
                report(ServerNotice::Message(format!(
                    "A viewer asked for {}.",
                    follower.selection()
                )));
            }
        }
        // Plugging, unplugging, or choosing another primary display does not
        // always interrupt Desktop Duplication, so compare the layout too.
        if change != DisplayChange::Restart
            && capture.is_some()
            && last_layout_check.elapsed() >= LAYOUT_CHECK_INTERVAL
        {
            last_layout_check = Instant::now();
            change = match follower.layout_read(monitor_layout(), attached_areas) {
                DisplayChange::None => change,
                DisplayChange::Restart => {
                    report(ServerNotice::Message(format!(
                        "The display layout changed; restarting capture of {}.",
                        follower.selection()
                    )));
                    DisplayChange::Restart
                }
                renumbered => renumbered,
            };
        }
        match change {
            DisplayChange::None => {}
            DisplayChange::Renumbered => {
                if let Some(plan) = follower.plan() {
                    server.set_serve_display(plan.announcement());
                    if let Some(notice) = plan.fallback_notice() {
                        report(ServerNotice::Message(notice));
                    }
                }
            }
            DisplayChange::Restart => {
                // A display can be duplicated once per process; stop first.
                outage.stopped(Instant::now());
                capture = None;
                follower.stopped();
                next_retry = Instant::now();
                retry_delay = CAPTURE_RETRY_MIN;
            }
        }

        let Some(active) = capture.as_mut() else {
            if Instant::now() < next_retry {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            // Capture stops when the desktop switches, as to the lock
            // screen; the agent duplicates the one that receives input now.
            if let Err(error) = follow_input_desktop()
                && error != last_capture_error
            {
                report(ServerNotice::Message(error.clone()));
                last_capture_error = error;
            }
            match start_capture(&mut follower) {
                Ok((recreated, plan)) => {
                    let size = (recreated.placement.width, recreated.placement.height);
                    if size != (framebuffer.width() as u16, framebuffer.height() as u16) {
                        framebuffer = Framebuffer::new(size.0, size.1)?;
                        served = ServedImage::new(size, scale)?;
                        let (width, height) = served.size(&framebuffer);
                        report(ServerNotice::Resized { width, height });
                    }
                    let (served_width, served_height) = served.size(&framebuffer);
                    *placement
                        .lock()
                        .map_err(|_| "display placement lock poisoned")? = DisplayPlacement {
                        served_width,
                        served_height,
                        ..recreated.placement
                    };
                    server.set_serve_display(plan.announcement());
                    report(ServerNotice::Message(
                        plan.fallback_notice()
                            .unwrap_or_else(|| format!("Serving {}.", plan.description())),
                    ));
                    capture = Some(recreated);
                    outage.resumed();
                    server.set_capture_status(CaptureStatus::Live);
                    retry_delay = CAPTURE_RETRY_MIN;
                    last_capture_error.clear();
                    last_layout_check = Instant::now();
                }
                Err(error) => {
                    outage.failed(capture_failure_status(error.as_ref()), Instant::now());
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
        damage.clear();
        if let Err(error) = active.next_frame(framebuffer.pixels_mut(), &mut damage) {
            // Desktop switches (UAC, lock screen), mode changes, unplugged
            // displays, and driver resets all invalidate duplication; keep
            // serving the last image and recreate capture.
            report(ServerNotice::Message(format!(
                "Desktop capture paused: {error}"
            )));
            last_capture_error = error.to_string();
            capture = None;
            follower.stopped();
            next_retry = Instant::now() + retry_delay;
            continue;
        }
        let requested_scale = f32::from(server.requested_serve_scale()) / 100.0;
        let scale_changed = requested_scale != scale;
        if scale_changed {
            scale = requested_scale;
            served = ServedImage::new((active.placement.width, active.placement.height), scale)?;
            let (width, height) = served.size(&framebuffer);
            let mut target = placement
                .lock()
                .map_err(|_| "display placement lock poisoned")?;
            target.served_width = width;
            target.served_height = height;
            report(ServerNotice::Resized { width, height });
        }
        if scale_changed {
            damage.clear();
            damage.push(Rect::new(
                0,
                0,
                framebuffer.width() as i32,
                framebuffer.height() as i32,
            ));
        }
        if !damage.is_empty() {
            served.update(&framebuffer, &damage);
            if !served.regions.is_empty() {
                server.update_framebuffer_regions(served.image(&framebuffer), &served.regions)?;
                server.set_serve_scale((scale * 100.0).round() as u8);
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

/// Run capture and input on the desktop that receives input, for the host
/// agent running as SYSTEM. `secure_attention` is called when a viewer
/// presses Ctrl+Alt+Delete.
pub fn follow_input_desktops(secure_attention: fn()) {
    let _ = SECURE_ATTENTION.set(secure_attention);
    FOLLOW_INPUT_DESKTOP.store(true, Ordering::Release);
}

/// In the host agent, move the calling thread to the desktop that receives
/// input, such as Winlogon for the lock screen. Only SYSTEM can open the
/// secure desktops. Returns whether the thread moved; does nothing outside
/// the agent.
fn follow_input_desktop() -> Result<bool, String> {
    /// One handle per desktop name, opened once and kept: a thread's
    /// desktop handle cannot be closed while the thread uses it.
    static DESKTOPS: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());
    if !FOLLOW_INPUT_DESKTOP.load(Ordering::Acquire) {
        return Ok(false);
    }
    let input = unsafe { OpenInputDesktop(0, 0, GENERIC_ALL) };
    if input.is_null() {
        return Err(format!(
            "could not open the desktop that receives input: {}",
            std::io::Error::last_os_error()
        ));
    }
    let name = desktop_name(input);
    let current = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
    if name.is_some() && name == desktop_name(current) {
        unsafe { CloseDesktop(input) };
        return Ok(false);
    }
    let desktop = match (name, DESKTOPS.lock()) {
        (Some(name), Ok(mut desktops)) => match desktops.iter().find(|(known, _)| *known == name) {
            Some(&(_, handle)) => {
                unsafe { CloseDesktop(input) };
                handle as HDESK
            }
            None => {
                desktops.push((name, input as usize));
                input
            }
        },
        // Unnamed desktops are not cached; this handle stays open.
        _ => input,
    };
    if unsafe { SetThreadDesktop(desktop) } == 0 {
        return Err(format!(
            "could not switch to the desktop that receives input: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(true)
}

/// A desktop's name, such as `Default` or `Winlogon`.
fn desktop_name(desktop: HDESK) -> Option<String> {
    if desktop.is_null() {
        return None;
    }
    let mut name = [0u16; 256];
    let mut needed = 0;
    let read = unsafe {
        GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            size_of_val(&name) as u32,
            &mut needed,
        )
    };
    (read != 0).then(|| device_name(&name))
}

/// Make this thread see physical pixels without changing the DPI awareness of
/// windows created on other threads.
fn enable_thread_dpi_awareness() {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn enable_dpi_awareness() {
    unsafe {
        // Fails when a manifest already chose an awareness mode; keep that one.
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            SetProcessDPIAware();
        }
    }
}

fn run_input(
    server: &VncServer,
    placement: &Mutex<DisplayPlacement>,
    remote_clipboard_sequence: &AtomicU32,
    shutdown: &AtomicBool,
    report: &(dyn Fn(ServerNotice) + Sync),
) {
    let mut input = InputReleaseGuard(RemoteInputState::default());
    let mut injection_failed = false;
    let mut desktop_checked: Option<Instant> = None;
    while !shutdown.load(Ordering::Acquire) {
        let event = match server.recv_event_timeout(INPUT_POLL_INTERVAL) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        // Input goes to this thread's desktop; follow the lock screen. A
        // failure is retried at the next check and shows as rejected input.
        if injection_failed
            || desktop_checked.is_none_or(|at| at.elapsed() >= INPUT_DESKTOP_CHECK_INTERVAL)
        {
            desktop_checked = Some(Instant::now());
            let _ = follow_input_desktop();
        }
        let mut injection = Ok(false);
        match event {
            ClientEvent::Key {
                client_id,
                keysym,
                down,
            } => {
                let key = windows_key_identity(keysym);
                let secure_attention = SECURE_ATTENTION
                    .get()
                    .filter(|_| down && is_secure_attention(key, input.0.held_keys()));
                if let Some(request) = secure_attention {
                    // Injected Ctrl+Alt+Delete does nothing; the service
                    // starts it. Delete is not pressed, so not released.
                    request();
                } else if input.0.key_event(client_id, key, down) {
                    injection = inject_key(key, down);
                }
            }
            ClientEvent::Pointer {
                client_id,
                buttons,
                x,
                y,
            } => {
                let transition = input.0.pointer_event(client_id, buttons);
                if let Ok(placement) = placement.lock() {
                    injection = inject_pointer(*placement, x, y, transition);
                }
            }
            ClientEvent::RelativePointer {
                client_id,
                buttons,
                dx,
                dy,
            } => {
                let transition = input.0.pointer_event(client_id, buttons);
                injection = inject_relative_pointer(dx, dy, transition);
            }
            ClientEvent::ClientDisconnected { client_id } => {
                let (released, previous, buttons) = input.0.disconnect(client_id);
                for key in released {
                    injection = inject_key(key, false).and(injection);
                }
                injection = send_inputs(&button_inputs(previous, buttons)).and(injection);
            }
            ClientEvent::ClipboardText { text, .. } => {
                match set_system_clipboard(&text) {
                    Ok(sequence) => remote_clipboard_sequence.store(sequence, Ordering::Release),
                    Err(error) => report(ServerNotice::Message(format!(
                        "Could not update the system clipboard: {error}"
                    ))),
                }
                if let Err(error) = server.set_clipboard_text(&text) {
                    report(ServerNotice::Message(format!(
                        "Could not notify other VNC clients about clipboard text: {error}"
                    )));
                }
            }
        }
        // Report the first failure without flooding the UI on mouse motion.
        // A rejected batch must remain visible until an actual injection succeeds.
        match injection {
            Err(error) if !injection_failed => {
                injection_failed = true;
                report(ServerNotice::Message(format!(
                    "Windows rejected remote input: {error}. Check whether the foreground app \
                     is elevated or the desktop is locked; reconnecting the viewer does not \
                     change Windows input permissions."
                )));
            }
            Err(_) => {}
            Ok(true) if injection_failed => {
                injection_failed = false;
                report(ServerNotice::Message(
                    "Remote input injection resumed.".into(),
                ));
            }
            Ok(_) => {}
        }
    }
}

/// Releases every remotely held key and button when the input thread exits.
struct InputReleaseGuard(RemoteInputState<KeyIdentity>);

impl Drop for InputReleaseGuard {
    fn drop(&mut self) {
        let (keys, buttons) = self.0.release_all();
        for key in keys {
            let _ = inject_key(key, false);
        }
        let _ = send_inputs(&button_inputs(buttons, 0));
    }
}

fn send_inputs(inputs: &[INPUT]) -> std::io::Result<bool> {
    if inputs.is_empty() {
        return Ok(false);
    }
    let (sent, error) = unsafe {
        SetLastError(0);
        let sent = SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        );
        (sent, GetLastError())
    };
    input_injection_result(inputs.len(), sent, error)
}

fn input_injection_result(expected: usize, sent: u32, error: u32) -> std::io::Result<bool> {
    if sent as usize == expected {
        return Ok(expected != 0);
    }
    // UIPI rejection can leave last-error at zero; do not call it success.
    Err(std::io::Error::other(format!(
        "SendInput accepted {sent}/{expected} events (Windows error {error}; \
         Windows may block injection without providing an error code)"
    )))
}

fn keyboard_input(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn mouse_input(dx: i32, dy: i32, data: i32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn inject_key(key: KeyIdentity, down: bool) -> std::io::Result<bool> {
    let key_up = if down { 0 } else { KEYEVENTF_KEYUP };
    match key {
        KeyIdentity::Virtual(VirtualKey { code, extended }) => {
            // Scan codes let games that read raw input or DirectInput see the key.
            let scan = unsafe { MapVirtualKeyW(u32::from(code), MAPVK_VK_TO_VSC) } as u16;
            let extended = if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
            send_inputs(&[keyboard_input(code, scan, extended | key_up)])
        }
        KeyIdentity::Unicode(keysym) => {
            let Some((first, second)) = unicode_key_units(keysym) else {
                return Ok(false);
            };
            let unit = |unit| keyboard_input(0, unit, KEYEVENTF_UNICODE | key_up);
            match (second, down) {
                (None, _) => send_inputs(&[unit(first)]),
                (Some(second), true) => send_inputs(&[unit(first), unit(second)]),
                (Some(second), false) => send_inputs(&[unit(second), unit(first)]),
            }
        }
    }
}

fn button_inputs(previous: u16, next: u16) -> Vec<INPUT> {
    [
        (BUTTON_LEFT, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        (
            BUTTON_MIDDLE,
            MOUSEEVENTF_MIDDLEDOWN,
            MOUSEEVENTF_MIDDLEUP,
            0,
        ),
        (BUTTON_RIGHT, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        (BUTTON_BACK, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON1),
        (BUTTON_FORWARD, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON2),
    ]
    .into_iter()
    .filter(|(mask, ..)| previous & mask != next & mask)
    .map(|(mask, down, up, data)| {
        mouse_input(
            0,
            0,
            i32::from(data),
            if next & mask != 0 { down } else { up },
        )
    })
    .collect()
}

/// Button and wheel inputs for a pointer event, after any motion.
fn change_inputs(transition: PointerTransition) -> Vec<INPUT> {
    let mut inputs = button_inputs(transition.previous, transition.buttons);
    if transition.vertical_notches != 0 {
        inputs.push(mouse_input(
            0,
            0,
            transition.vertical_notches * WHEEL_DELTA,
            MOUSEEVENTF_WHEEL,
        ));
    }
    if transition.horizontal_notches != 0 {
        inputs.push(mouse_input(
            0,
            0,
            transition.horizontal_notches * WHEEL_DELTA,
            MOUSEEVENTF_HWHEEL,
        ));
    }
    inputs
}

/// Move the pointer by (`dx`, `dy`) without `MOUSEEVENTF_ABSOLUTE`, so games
/// reading raw input receive exactly these deltas.
fn inject_relative_pointer(
    dx: i32,
    dy: i32,
    transition: PointerTransition,
) -> std::io::Result<bool> {
    let mut inputs = Vec::new();
    if (dx, dy) != (0, 0) {
        inputs.push(mouse_input(dx, dy, 0, MOUSEEVENTF_MOVE));
    }
    inputs.extend(change_inputs(transition));
    send_inputs(&inputs)
}

/// Whether the system cursor is shown. While a game hides it to turn the
/// camera with the mouse, this is false. Touch input suppresses the cursor
/// without hiding it, and failures (such as on the secure desktop) count as
/// shown.
fn cursor_showing() -> bool {
    let mut info = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetCursorInfo(&mut info) } == 0 {
        return true;
    }
    info.flags & CURSOR_SUPPRESSED != 0
        || (info.flags & CURSOR_SHOWING != 0 && !info.hCursor.is_null())
}

/// Scheduling for a host whose game runs in the foreground: 1 ms timer
/// resolution, no power throttling (EcoQoS would move encoding to efficiency
/// cores and stretch timed waits), and a high priority class. Restored on
/// drop. Each step is best effort.
struct HostScheduling {
    timer_period: bool,
    previous_class: u32,
}

impl HostScheduling {
    fn raise(report: &(dyn Fn(ServerNotice) + Sync)) -> Self {
        let timer_period = unsafe { timeBeginPeriod(1) } == TIMERR_NOERROR;
        let process = unsafe { GetCurrentProcess() };
        let throttling = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            // Opted out of both.
            StateMask: 0,
        };
        let unthrottled = unsafe {
            SetProcessInformation(
                process,
                ProcessPowerThrottling,
                (&throttling as *const PROCESS_POWER_THROTTLING_STATE).cast(),
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        } != 0;
        let previous_class = unsafe { GetPriorityClass(process) };
        let raised = unsafe { SetPriorityClass(process, HIGH_PRIORITY_CLASS) } != 0;
        if !(timer_period && unthrottled && raised) {
            report(ServerNotice::Message(
                "Could not fully raise host scheduling priority; capture and encoding may lag behind a busy game."
                    .into(),
            ));
        }
        Self {
            timer_period,
            previous_class: if raised { previous_class } else { 0 },
        }
    }
}

impl Drop for HostScheduling {
    fn drop(&mut self) {
        unsafe {
            if self.timer_period {
                timeEndPeriod(1);
            }
            if self.previous_class != 0 {
                SetPriorityClass(GetCurrentProcess(), self.previous_class);
            }
        }
    }
}

fn inject_pointer(
    placement: DisplayPlacement,
    x: u16,
    y: u16,
    transition: PointerTransition,
) -> std::io::Result<bool> {
    // Served pixels map to the display pixel under their center; this also
    // clamps a framebuffer briefly larger than a display that just shrank.
    let x = placement.left
        + i32::from(native_coordinate(
            x,
            placement.served_width,
            placement.width,
        ));
    let y = placement.top
        + i32::from(native_coordinate(
            y,
            placement.served_height,
            placement.height,
        ));
    let (virtual_left, virtual_top, virtual_width, virtual_height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let mut inputs = vec![mouse_input(
        absolute_mouse_coordinate(x, virtual_left, virtual_width),
        absolute_mouse_coordinate(y, virtual_top, virtual_height),
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )];
    inputs.extend(change_inputs(transition));
    send_inputs(&inputs)
}

fn read_system_clipboard_latin1() -> Result<Vec<u8>, Box<dyn Error>> {
    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            opened = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    if !opened {
        return Err("could not open the system clipboard".into());
    }
    let result = (|| {
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
        if handle.is_null() {
            return Ok(Vec::new());
        }
        let byte_len = unsafe { GlobalSize(handle) };
        if byte_len > 2 * (MAX_CLIPBOARD_CHARS + 1) {
            return Err("system clipboard text exceeds capture limits".into());
        }
        let source = unsafe { GlobalLock(handle) };
        if source.is_null() {
            return Err("could not lock system clipboard text".into());
        }
        let words = byte_len / size_of::<u16>();
        let utf16 = unsafe { std::slice::from_raw_parts(source.cast::<u16>(), words) };
        let end = utf16.iter().position(|unit| *unit == 0).unwrap_or(words);
        let text = String::from_utf16_lossy(&utf16[..end]);
        unsafe { GlobalUnlock(handle) };
        Ok(latin1_from_unicode(&text))
    })();
    unsafe { CloseClipboard() };
    result
}

fn set_system_clipboard(text: &[u8]) -> Result<u32, Box<dyn Error>> {
    set_clipboard_utf16(&latin1_to_utf16(text))
}

/// Put Unicode text from the TopVNC window, such as the WEB tab's app URL,
/// on the system clipboard.
pub fn copy_text(text: &str) -> Result<(), String> {
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    set_clipboard_utf16(&utf16)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// `utf16` ends with its NUL, as CF_UNICODETEXT requires.
fn set_clipboard_utf16(utf16: &[u16]) -> Result<u32, Box<dyn Error>> {
    let byte_len = utf16
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or("clipboard text size overflow")?;
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) };
    if memory.is_null() {
        return Err("could not allocate clipboard memory".into());
    }
    let destination = unsafe { GlobalLock(memory) };
    if destination.is_null() {
        unsafe { GlobalFree(memory) };
        return Err("could not lock clipboard memory".into());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(utf16.as_ptr().cast::<u8>(), destination.cast(), byte_len);
        GlobalUnlock(memory);
    }
    // SetClipboardData fails unless the clipboard was opened by a window.
    let owner_class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
    let owner_title = "TopVNC clipboard\0".encode_utf16().collect::<Vec<_>>();
    let owner = unsafe {
        CreateWindowExW(
            0,
            owner_class.as_ptr(),
            owner_title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if owner.is_null() {
        unsafe { GlobalFree(memory) };
        return Err("could not create a clipboard owner window".into());
    }
    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(owner) } != 0 {
            opened = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    if !opened {
        unsafe {
            GlobalFree(memory);
            DestroyWindow(owner);
        }
        return Err("could not open the system clipboard".into());
    }
    let transferred = unsafe {
        let emptied = EmptyClipboard() != 0;
        emptied && !SetClipboardData(CF_UNICODETEXT as u32, memory).is_null()
    };
    unsafe {
        CloseClipboard();
        DestroyWindow(owner);
    }
    if !transferred {
        unsafe { GlobalFree(memory) };
        return Err("could not set clipboard text".into());
    }
    Ok(unsafe { GetClipboardSequenceNumber() })
}

/// Desktop Duplication of the selected displays. Each display is captured on
/// its own thread, so a display without changes never delays another, into
/// one image arranged as on the desktop. Any failure invalidates the whole
/// capture; the serving loop recreates it from scratch.
struct DesktopCapture {
    shared: Arc<CaptureShared>,
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
    /// The area the captured image covers on the virtual desktop.
    placement: DisplayPlacement,
    /// Windows raised every capture device's GPU thread priority, so copies
    /// do not queue behind a game's rendering.
    gpu_priority_raised: bool,
}

/// What capture threads hand to the serving loop.
struct CaptureShared {
    state: Mutex<CaptureState>,
    changed: Condvar,
}

impl CaptureShared {
    fn new(width: u16, height: u16, outputs: usize) -> Self {
        Self {
            state: Mutex::new(CaptureState {
                image: DesktopImage::new(width, height),
                // The first present covers areas no display fills, too.
                captured: vec![Rect::new(0, 0, i32::from(width), i32::from(height))],
                changed: false,
                first_frames_pending: outputs,
                pointer: PointerOwner::default(),
                error: None,
            }),
            changed: Condvar::new(),
        }
    }

    /// Duplication can start successfully on an output that never delivers
    /// an image. Establish a complete first image before declaring it live.
    fn wait_for_first_frames(&self, timeout: Duration) -> Result<(), Box<dyn Error>> {
        let state = self
            .state
            .lock()
            .map_err(|_| "capture state lock poisoned")?;
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                state.first_frames_pending != 0 && state.error.is_none()
            })
            .map_err(|_| "capture state lock poisoned")?;
        if let Some(error) = state.error.take() {
            return Err(error.into());
        }
        if state.first_frames_pending != 0 {
            return Err(format!(
                "{} display(s) did not deliver their first desktop image before the capture timeout",
                state.first_frames_pending
            )
            .into());
        }
        Ok(())
    }
}

struct CaptureState {
    image: DesktopImage,
    /// Image regions captured since the serving loop last presented.
    captured: Vec<Rect>,
    /// The image or cursor changed since the serving loop last presented.
    changed: bool,
    /// Decremented once per output, after its first full image is copied.
    first_frames_pending: usize,
    pointer: PointerOwner,
    /// Why a display's capture stopped; the whole capture is then recreated.
    error: Option<String>,
}

/// Capture cannot start for a reason viewers are told.
#[derive(Debug)]
struct CaptureUnavailable {
    status: CaptureStatus,
    message: String,
}

impl std::fmt::Display for CaptureUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CaptureUnavailable {}

/// What viewers are told about a capture that failed to start.
fn capture_failure_status(error: &(dyn Error + 'static)) -> CaptureStatus {
    error
        .downcast_ref::<CaptureUnavailable>()
        .map_or(CaptureStatus::Unavailable, |unavailable| unavailable.status)
}

/// Start capturing what `follower` selects, among the displays attached
/// now, and tell it what is served.
fn start_capture(
    follower: &mut DisplayFollower,
) -> Result<(DesktopCapture, DisplayPlan), Box<dyn Error>> {
    // Read the layout first: a change while starting is then noticed.
    let layout = monitor_layout();
    let attached = attached_outputs_in(&layout)?;
    if attached.is_empty() {
        return Err(Box::new(CaptureUnavailable {
            status: CaptureStatus::NoDisplay,
            message: "no display with a connected monitor was found".into(),
        }));
    }
    let areas = attached
        .iter()
        .map(|output| output.area.clone())
        .collect::<Vec<_>>();
    let (capture, plan) = capture_selected_or_primary(
        follower.selection(),
        &areas,
        |plan| DesktopCapture::new(plan, &attached),
        |capture| capture.shared.wait_for_first_frames(FIRST_FRAME_TIMEOUT),
    )?;
    follower.started(plan.clone(), layout);
    Ok((capture, plan))
}

/// An unplugged output can remain enumerated while duplication already
/// fails. Try the current primary before leaving viewers without capture.
fn capture_selected_or_primary<C>(
    selection: DisplaySelection,
    attached: &[DisplayArea],
    mut capture: impl FnMut(&DisplayPlan) -> Result<C, Box<dyn Error>>,
    mut ready: impl FnMut(&C) -> Result<(), Box<dyn Error>>,
) -> Result<(C, DisplayPlan), Box<dyn Error>> {
    let plan = DisplayPlan::new(selection, attached)?;
    let mut attempt = |plan: &DisplayPlan| {
        let captured = capture(plan)?;
        // Dropping a failed attempt releases its duplication before the
        // primary is tried, including when its first acquisition failed.
        ready(&captured)?;
        Ok::<_, Box<dyn Error>>(captured)
    };
    let error = match attempt(&plan) {
        Ok(captured) => return Ok((captured, plan)),
        Err(error) => error,
    };
    // A secure desktop requires the service's privileges; another output
    // cannot make it accessible.
    if capture_failure_status(error.as_ref()) == CaptureStatus::SecureDesktop {
        return Err(error);
    }
    let mut primary = DisplayPlan::new(DisplaySelection::Primary, attached)?;
    if primary.outputs == plan.outputs {
        return Err(error);
    }
    primary.chosen.fallback = Some(format!("could not capture {}: {error}", plan.description()));
    match attempt(&primary) {
        Ok(captured) => Ok((captured, primary)),
        Err(fallback_error) => Err(Box::new(CaptureUnavailable {
            status: capture_failure_status(fallback_error.as_ref()),
            message: format!(
                "could not capture {}: {error}; fallback to {} also failed: {fallback_error}",
                plan.description(),
                primary.description()
            ),
        })),
    }
}

/// The attached displays, for the display follower.
fn attached_areas() -> Result<Vec<DisplayArea>, String> {
    attached_outputs()
        .map(|outputs| outputs.into_iter().map(|output| output.area).collect())
        .map_err(|error| error.to_string())
}

impl DesktopCapture {
    /// Capture the displays `plan` chose from `attached`.
    fn new(plan: &DisplayPlan, attached: &[AttachedOutput]) -> Result<Self, Box<dyn Error>> {
        let bounds = plan.chosen.bounds;
        let width = u16::try_from(bounds.right - bounds.left)
            .map_err(|_| "display width is outside VNC limits")?;
        let height = u16::try_from(bounds.bottom - bounds.top)
            .map_err(|_| "display height is outside VNC limits")?;
        validate_capture_dimensions(width, height)?;
        let mut captures = Vec::new();
        for (owner, &index) in plan.chosen.indices.iter().enumerate() {
            let output = attached
                .get(index)
                .ok_or("a planned display is not attached")?;
            let rect = output.area.rect;
            let origin = (
                (rect.left - bounds.left) as usize,
                (rect.top - bounds.top) as usize,
            );
            captures.push(OutputCapture::new(output, owner, origin)?);
        }
        let mut capture = Self {
            shared: Arc::new(CaptureShared::new(width, height, captures.len())),
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            placement: DisplayPlacement {
                left: bounds.left,
                top: bounds.top,
                width,
                height,
                served_width: width,
                served_height: height,
            },
            gpu_priority_raised: captures.iter().all(|output| output.gpu_priority_raised),
        };
        for output in captures {
            let shared = Arc::clone(&capture.shared);
            let stop = Arc::clone(&capture.stop);
            // Dropping `capture` on a failed spawn stops the threads started.
            capture.threads.push(
                thread::Builder::new()
                    .name("topvnc-capture".into())
                    .spawn(move || capture_output(output, &shared, &stop))?,
            );
        }
        Ok(capture)
    }

    /// Wait up to one frame interval for desktop or pointer changes, then
    /// draw them into `output`, the captured-size framebuffer pixels, and
    /// append the regions that changed to `damage`.
    fn next_frame(&self, output: &mut [u32], damage: &mut Vec<Rect>) -> Result<(), Box<dyn Error>> {
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| "capture state lock poisoned")?;
        let (mut state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, FRAME_TIMEOUT, |state| {
                !state.changed && state.error.is_none()
            })
            .map_err(|_| "capture state lock poisoned")?;
        if let Some(error) = state.error.take() {
            return Err(error.into());
        }
        if state.changed {
            let state = &mut *state;
            state.image.present(&state.captured, output, damage);
            state.captured.clear();
            state.changed = false;
        }
        Ok(())
    }
}

impl Drop for DesktopCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Capture one display until stopped or until its capture fails.
fn capture_output(mut output: OutputCapture, shared: &CaptureShared, stop: &AtomicBool) {
    enable_thread_dpi_awareness();
    // A failure shows up as a lost duplication, which restarts capture.
    let _ = follow_input_desktop();
    while !stop.load(Ordering::Acquire) {
        if let Err(error) = output.next_frame(shared) {
            if let Ok(mut state) = shared.state.lock() {
                state.error.get_or_insert_with(|| error.to_string());
            }
            shared.changed.notify_all();
            return;
        }
    }
}

/// An attached display output and its place on the desktop.
struct AttachedOutput {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
    area: DisplayArea,
    rotation: DXGI_MODE_ROTATION,
}

/// Desktop Duplication of one display output.
struct OutputCapture {
    /// This display's position among the captured ones, for cursor ownership.
    owner: usize,
    /// Where this display's top-left pixel is in the captured image.
    origin: (usize, usize),
    device: ID3D11Device,
    gpu_priority_raised: bool,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    rotation: Rotation,
    source_width: u16,
    source_height: u16,
    /// The next frame must be copied in full.
    full_frame_pending: bool,
    pointer_shape: Vec<u8>,
    move_rects: Vec<DXGI_OUTDUPL_MOVE_RECT>,
    dirty_rects: Vec<RECT>,
    source_rects: Vec<Rect>,
}

impl OutputCapture {
    fn new(
        attached: &AttachedOutput,
        owner: usize,
        origin: (usize, usize),
    ) -> Result<Self, Box<dyn Error>> {
        let rect = attached.area.rect;
        let width = u16::try_from(rect.right - rect.left)
            .map_err(|_| "display width is outside VNC limits")?;
        let height = u16::try_from(rect.bottom - rect.top)
            .map_err(|_| "display height is outside VNC limits")?;
        validate_capture_dimensions(width, height)?;
        let rotation = rotation(attached.rotation)?;
        let (source_width, source_height) = rotation.source_size(width, height);
        let adapter = &attached.adapter;
        let feature_levels = [
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_10_0,
        ];
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE(std::ptr::null_mut()),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|error| format!("could not create the display capture device: {error}"))?;
        }
        let device = device.ok_or("Direct3D did not return a capture device")?;
        let context = context.ok_or("Direct3D did not return a device context")?;
        let gpu_priority_raised = device
            .cast::<IDXGIDevice>()
            .is_ok_and(|device| unsafe { device.SetGPUThreadPriority(7) }.is_ok());
        let output: IDXGIOutput1 = attached.output.cast()?;
        let duplication =
            unsafe { output.DuplicateOutput(&device) }.map_err(|error| -> Box<dyn Error> {
                if error.code() == E_ACCESSDENIED {
                    Box::new(CaptureUnavailable {
                        status: CaptureStatus::SecureDesktop,
                        message: format!(
                            "Windows denied access to the desktop (secure desktop or locked session): {error}"
                        ),
                    })
                } else {
                    format!("could not start Desktop Duplication: {error}").into()
                }
            })?;
        Ok(Self {
            owner,
            origin,
            device,
            gpu_priority_raised,
            context,
            duplication,
            staging: None,
            rotation,
            source_width,
            source_height,
            full_frame_pending: true,
            pointer_shape: Vec::new(),
            move_rects: Vec::new(),
            dirty_rects: Vec::new(),
            source_rects: Vec::new(),
        })
    }

    /// Wait up to one frame interval for desktop or pointer changes, and
    /// copy them into the shared image.
    fn next_frame(&mut self, shared: &CaptureShared) -> Result<(), Box<dyn Error>> {
        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        if let Err(error) = unsafe {
            self.duplication
                .AcquireNextFrame(FRAME_TIMEOUT_MS, &mut frame_info, &mut resource)
        } {
            if error.code() == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(());
            }
            return Err(format!("could not acquire a desktop frame: {error}").into());
        }
        let _frame = FrameGuard(self.duplication.clone());
        let shape = match frame_info.PointerShapeBufferSize {
            0 => None,
            size => Some(self.read_pointer_shape(size)?),
        };
        // Only the pointer changed when nothing was presented.
        let texture = if frame_info.LastPresentTime != 0 || self.full_frame_pending {
            let resource = resource.ok_or("Desktop Duplication returned no frame resource")?;
            let texture: ID3D11Texture2D = resource.cast()?;
            let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { texture.GetDesc(&mut texture_desc) };
            if texture_desc.Width != u32::from(self.source_width)
                || texture_desc.Height != u32::from(self.source_height)
            {
                return Err("display mode changed".into());
            }
            if texture_desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
                return Err("desktop frames are not 32-bit BGRA".into());
            }
            self.collect_source_rects(&frame_info)?;
            (!self.source_rects.is_empty()).then_some(texture)
        } else {
            None
        };
        let staging = match &texture {
            Some(texture) => Some(self.copy_to_staging(texture)?),
            None => None,
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let _mapped = match &staging {
            Some(staging) => {
                unsafe {
                    self.context
                        .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                }
                Some(MappedTexture {
                    context: &self.context,
                    texture: staging,
                })
            }
            None => None,
        };
        let surface = match &_mapped {
            Some(_) => Some(mapped_surface(
                &mapped,
                self.source_width,
                self.source_height,
                self.rotation,
            )?),
            None => None,
        };

        let mut state = shared
            .state
            .lock()
            .map_err(|_| "capture state lock poisoned")?;
        let state = &mut *state;
        if frame_info.LastMouseUpdateTime != 0 {
            let position = frame_info.PointerPosition.Position;
            let visible = frame_info.PointerPosition.Visible.as_bool();
            if state
                .pointer
                .accept(self.owner, visible, frame_info.LastMouseUpdateTime)
            {
                // Positions are relative to this display.
                state.image.set_cursor_position(
                    self.origin.0 as i32 + position.x,
                    self.origin.1 as i32 + position.y,
                    visible,
                );
            }
        }
        if let Some(shape) = shape {
            state.image.set_cursor_shape(shape);
        }
        if let Some(surface) = &surface {
            for rect in &self.source_rects {
                let desktop = self.rotation.desktop_rect(
                    *rect,
                    i32::from(self.source_width),
                    i32::from(self.source_height),
                );
                if let Some(changed) = state.image.copy_from_at(surface, desktop, self.origin) {
                    state.captured.push(changed);
                }
            }
            if state.captured.len() > MAX_PENDING_RECTS {
                state.captured.clear();
                state.captured.push(state.image.bounds());
            }
            if self.full_frame_pending {
                state.first_frames_pending -= 1;
            }
            self.full_frame_pending = false;
        }
        state.changed = true;
        shared.changed.notify_all();
        Ok(())
    }

    /// Copy the changed regions of `texture` to the CPU-readable staging
    /// texture.
    fn copy_to_staging(
        &mut self,
        texture: &ID3D11Texture2D,
    ) -> Result<ID3D11Texture2D, Box<dyn Error>> {
        let staging = self.staging_texture()?;
        if self.source_rects.len() > MAX_REGION_COPIES {
            unsafe { self.context.CopyResource(&staging, texture) };
        } else {
            for rect in &self.source_rects {
                let region = D3D11_BOX {
                    left: rect.left as u32,
                    top: rect.top as u32,
                    front: 0,
                    right: rect.right as u32,
                    bottom: rect.bottom as u32,
                    back: 1,
                };
                unsafe {
                    self.context.CopySubresourceRegion(
                        &staging,
                        0,
                        region.left,
                        region.top,
                        0,
                        texture,
                        0,
                        Some(&region),
                    );
                }
            }
        }
        Ok(staging)
    }

    /// Collect changed regions of the captured image, clipped to its bounds.
    fn collect_source_rects(
        &mut self,
        frame_info: &DXGI_OUTDUPL_FRAME_INFO,
    ) -> Result<(), Box<dyn Error>> {
        let bounds = Rect::new(
            0,
            0,
            i32::from(self.source_width),
            i32::from(self.source_height),
        );
        self.source_rects.clear();
        if self.full_frame_pending || frame_info.TotalMetadataBufferSize == 0 {
            self.source_rects.push(bounds);
            return Ok(());
        }
        if frame_info.TotalMetadataBufferSize > MAX_FRAME_METADATA_BYTES {
            return Err("desktop frame metadata exceeds capture limits".into());
        }
        let capacity = frame_info.TotalMetadataBufferSize as usize;
        // Moved regions already hold their new pixels in the frame, so their
        // destinations are refreshed like dirty regions.
        let moves = read_metadata(
            &mut self.move_rects,
            capacity,
            |size, buffer, required| unsafe {
                self.duplication.GetFrameMoveRects(size, buffer, required)
            },
        )?;
        let dirty = read_metadata(
            &mut self.dirty_rects,
            capacity,
            |size, buffer, required| unsafe {
                self.duplication.GetFrameDirtyRects(size, buffer, required)
            },
        )?;
        let rects = self.move_rects[..moves]
            .iter()
            .map(|moved| moved.DestinationRect)
            .chain(self.dirty_rects[..dirty].iter().copied());
        for rect in rects {
            if let Some(rect) =
                Rect::new(rect.left, rect.top, rect.right, rect.bottom).intersect(bounds)
            {
                self.source_rects.push(rect);
            }
        }
        Ok(())
    }

    fn read_pointer_shape(&mut self, size: u32) -> Result<Option<CursorShape>, Box<dyn Error>> {
        if size > MAX_POINTER_SHAPE_BYTES {
            return Err("desktop pointer shape exceeds capture limits".into());
        }
        self.pointer_shape.resize(size as usize, 0);
        let mut required = 0;
        let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        unsafe {
            self.duplication.GetFramePointerShape(
                size,
                self.pointer_shape.as_mut_ptr().cast(),
                &mut required,
                &mut info,
            )?;
        }
        let kind = match info.Type as i32 {
            value if value == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 => {
                CursorKind::Monochrome
            }
            value if value == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 => CursorKind::Color,
            value if value == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 => {
                CursorKind::MaskedColor
            }
            _ => return Ok(None),
        };
        let data = self.pointer_shape[..(required.min(size)) as usize].to_vec();
        // An unusable shape hides the software cursor rather than stopping capture.
        Ok(CursorShape::new(kind, info.Width, info.Height, info.Pitch, data).ok())
    }

    fn staging_texture(&mut self) -> Result<ID3D11Texture2D, Box<dyn Error>> {
        let (width, height) = (u32::from(self.source_width), u32::from(self.source_height));
        if let Some((texture, staged_width, staged_height)) = &self.staging
            && (*staged_width, *staged_height) == (width, height)
        {
            return Ok(texture.clone());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut staging))?
        };
        let staging = staging.ok_or("Direct3D did not create a staging texture")?;
        self.staging = Some((staging.clone(), width, height));
        Ok(staging)
    }
}

/// Read one kind of frame metadata into `buffer`, growing it when DXGI asks
/// for more space. Returns the number of entries.
fn read_metadata<T: Default + Clone>(
    buffer: &mut Vec<T>,
    capacity_bytes: usize,
    mut read: impl FnMut(u32, *mut T, &mut u32) -> windows::core::Result<()>,
) -> Result<usize, Box<dyn Error>> {
    let entry = size_of::<T>();
    buffer.resize(capacity_bytes.div_ceil(entry).max(1), T::default());
    for _ in 0..2 {
        let mut required = 0;
        let size = (buffer.len() * entry) as u32;
        match read(size, buffer.as_mut_ptr(), &mut required) {
            Ok(()) => return Ok((required as usize / entry).min(buffer.len())),
            Err(error) if error.code() == DXGI_ERROR_MORE_DATA => {
                if required > MAX_FRAME_METADATA_BYTES {
                    return Err("desktop frame metadata exceeds capture limits".into());
                }
                buffer.resize((required as usize).div_ceil(entry), T::default());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err("desktop frame metadata kept growing".into())
}

/// The pixels of a mapped staging texture of `width` x `height` pixels.
fn mapped_surface(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: u16,
    height: u16,
    rotation: Rotation,
) -> Result<CaptureSurface<'_>, Box<dyn Error>> {
    let (width, height) = (usize::from(width), usize::from(height));
    let row_pitch = mapped.RowPitch as usize;
    if mapped.pData.is_null() || row_pitch < width * 4 || height == 0 {
        return Err("Desktop Duplication returned an invalid row layout".into());
    }
    // SAFETY: the mapped staging texture holds `height` rows of `row_pitch`
    // bytes, the last of which has at least `width` pixels, and stays mapped
    // while `mapped` is borrowed.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            mapped.pData.cast::<u8>(),
            row_pitch * (height - 1) + width * 4,
        )
    };
    Ok(CaptureSurface {
        bytes,
        row_pitch,
        width,
        height,
        rotation,
    })
}

fn device_name(name: &[u16]) -> String {
    let len = name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(name.len());
    String::from_utf16_lossy(&name[..len])
}

/// The displays `--display` and the Server tab number, in DXGI enumeration
/// order, with their desktop areas in physical pixels. Empty when they
/// cannot be listed.
pub fn displays() -> Vec<DisplayArea> {
    // The UI thread keeps its own DPI awareness; areas are read in physical
    // pixels, as capture uses them.
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let displays = attached_outputs()
        .map(|outputs| outputs.into_iter().map(|output| output.area).collect())
        .unwrap_or_default();
    if !previous.is_null() {
        unsafe { SetThreadDpiAwarenessContext(previous as DPI_AWARENESS_CONTEXT) };
    }
    displays
}

/// Display outputs in the current desktop layout, in DXGI enumeration order.
fn attached_outputs() -> Result<Vec<AttachedOutput>, Box<dyn Error>> {
    attached_outputs_in(&monitor_layout())
}

fn attached_outputs_in(layout: &[DisplayArea]) -> Result<Vec<AttachedOutput>, Box<dyn Error>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut outputs = Vec::new();
    'adapters: for adapter_index in 0..16 {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(error.into()),
        };
        for output_index in 0..16 {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(error.into()),
            };
            let desc: DXGI_OUTPUT_DESC = unsafe { output.GetDesc()? };
            let name = device_name(&desc.DeviceName);
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let rect = desc.DesktopCoordinates;
            let Some(area) = current_output_area(
                &name,
                Rect::new(rect.left, rect.top, rect.right, rect.bottom),
                layout,
            ) else {
                continue;
            };
            outputs.push(AttachedOutput {
                adapter: adapter.clone(),
                output,
                area: area.clone(),
                rotation: desc.Rotation,
            });
            if outputs.len() == MAX_DISPLAYS {
                break 'adapters;
            }
        }
    }
    validate_output_layout(layout, outputs.iter().map(|output| &output.area))?;
    Ok(outputs)
}

/// Resolving a partial list could change a numbered selection or keep ALL
/// on a subset forever: GDI may already be steady while DXGI catches up.
fn validate_output_layout<'a>(
    layout: &[DisplayArea],
    output_areas: impl Iterator<Item = &'a DisplayArea>,
) -> Result<(), Box<dyn Error>> {
    let outputs = output_areas.collect::<Vec<_>>();
    if let Some(missing) = layout.iter().find(|area| {
        !outputs
            .iter()
            .any(|output| output.name.eq_ignore_ascii_case(&area.name) && output.rect == area.rect)
    }) {
        return Err(format!(
            "display {} at {:?} is not yet available in DXGI; waiting for Windows to finish reconfiguring displays",
            missing.name, missing.rect
        )
        .into());
    }
    Ok(())
}

/// GDI can already have the new main display while DXGI still lists the
/// removed output at the origin. Wait for matching geometry, and use GDI's
/// primary flag rather than inferring it from stale DXGI coordinates.
fn current_output_area<'a>(
    name: &str,
    rect: Rect,
    layout: &'a [DisplayArea],
) -> Option<&'a DisplayArea> {
    layout
        .iter()
        .find(|area| area.name.eq_ignore_ascii_case(name) && area.rect == rect)
}

/// GDI device names, in upper case, of the desktop displays at least one
/// connected monitor shows, or `None` when Windows cannot say. Windows can
/// keep an unplugged monitor's display on the desktop, marked unavailable,
/// for a while or for good; Desktop Duplication of it then waits for frames
/// that never come, without an error.
fn connected_sources() -> Option<HashSet<String>> {
    // The topology can change between sizing and reading; try again, and
    // give up (keeping every display) if it keeps changing.
    let mut active = None;
    for _ in 0..4 {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        if unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        } != ERROR_SUCCESS
        {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        match unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        } {
            ERROR_SUCCESS => {
                paths.truncate(path_count as usize);
                active = Some(paths);
                break;
            }
            ERROR_INSUFFICIENT_BUFFER => {}
            _ => return None,
        }
    }
    let paths = active?;
    let mut connected = HashSet::new();
    for path in paths
        .iter()
        .filter(|path| path.targetInfo.targetAvailable != 0)
    {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != ERROR_SUCCESS as i32 {
            return None;
        }
        connected.insert(device_name(&source.viewGdiDeviceName).to_ascii_uppercase());
    }
    Some(connected)
}

/// The monitors on the desktop as GDI reports them, without displays no
/// connected monitor shows. Cheap enough to compare twice a second, unlike
/// enumerating DXGI outputs.
fn monitor_layout() -> Vec<DisplayArea> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // SAFETY: `data` is the vector passed to EnumDisplayMonitors below,
        // which calls back synchronously.
        let monitors = unsafe { &mut *(data.0 as *mut Vec<DisplayArea>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if unsafe {
            GetMonitorInfoW(
                monitor,
                (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>(),
            )
        }
        .as_bool()
        {
            let area = info.monitorInfo.rcMonitor;
            monitors.push(DisplayArea {
                name: device_name(&info.szDevice),
                rect: Rect::new(area.left, area.top, area.right, area.bottom),
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        BOOL::from(monitors.len() < MAX_MONITORS)
    }
    let mut monitors = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut monitors as *mut Vec<DisplayArea> as isize),
        );
    }
    connected_displays(monitors, connected_sources().as_ref())
}

fn rotation(rotation: DXGI_MODE_ROTATION) -> Result<Rotation, &'static str> {
    match rotation {
        DXGI_MODE_ROTATION_IDENTITY | DXGI_MODE_ROTATION_UNSPECIFIED => Ok(Rotation::Identity),
        DXGI_MODE_ROTATION_ROTATE90 => Ok(Rotation::Rotate90),
        DXGI_MODE_ROTATION_ROTATE180 => Ok(Rotation::Rotate180),
        DXGI_MODE_ROTATION_ROTATE270 => Ok(Rotation::Rotate270),
        _ => Err("unsupported display rotation"),
    }
}

struct MappedTexture<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
}

impl Drop for MappedTexture<'_> {
    fn drop(&mut self) {
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}

/// Releases an acquired frame; holds its own reference to the duplication.
struct FrameGuard(IDXGIOutputDuplication);

impl Drop for FrameGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.ReleaseFrame();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_host::choose_displays;
    use topvnc::ServedDisplays;

    fn recovery_displays() -> Vec<DisplayArea> {
        vec![
            DisplayArea {
                name: "\\\\.\\DISPLAY5".into(),
                rect: Rect::new(-5120, 0, 0, 1440),
                primary: false,
            },
            DisplayArea {
                name: "\\\\.\\DISPLAY1".into(),
                rect: Rect::new(0, 0, 2560, 1600),
                primary: true,
            },
        ]
    }

    #[test]
    fn partial_dxgi_snapshots_are_retried_before_numbering_or_all_selection() {
        let mut layout = recovery_displays();
        layout.push(DisplayArea {
            name: "\\\\.\\DISPLAY3".into(),
            rect: Rect::new(2560, 0, 4480, 1080),
            primary: false,
        });
        // The primary is already current, but another output has not caught
        // up. Accepting this list would move DISPLAY3 into display 2's slot.
        let partial = [&layout[1], &layout[2]];
        let error = validate_output_layout(&layout, partial.into_iter()).unwrap_err();
        assert!(error.to_string().contains(&layout[0].name));
        // A complete snapshot is accepted without imposing GDI's order on
        // DXGI numbering. The empty case is classified as NoDisplay later.
        assert!(validate_output_layout(&layout, layout.iter().rev()).is_ok());
        assert!(validate_output_layout(&[], [].iter()).is_ok());
        let mut stale = layout.clone();
        stale[0].rect = Rect::new(0, 0, 5120, 1440);
        assert!(validate_output_layout(&layout, stale.iter()).is_err());
    }

    #[test]
    fn startup_requires_a_first_image_from_every_selected_output() {
        let shared = CaptureShared::new(4, 2, 2);
        assert!(shared.wait_for_first_frames(Duration::ZERO).is_err());
        {
            let mut state = shared.state.lock().unwrap();
            // A cursor change or an image from only one output cannot make
            // a combined capture live.
            state.changed = true;
            state.first_frames_pending = 1;
        }
        assert!(shared.wait_for_first_frames(Duration::ZERO).is_err());
        shared.state.lock().unwrap().first_frames_pending = 0;
        shared.wait_for_first_frames(Duration::ZERO).unwrap();
        // Waiting for readiness must leave the first image pending for the
        // serving loop, including black gaps between monitors.
        let state = shared.state.lock().unwrap();
        assert!(state.changed);
        assert_eq!(state.captured, [Rect::new(0, 0, 4, 2)]);
    }

    #[test]
    fn first_frame_failure_and_timeout_follow_the_same_primary_fallback() {
        let attached = recovery_displays();
        for selection in [DisplaySelection::Number(1), DisplaySelection::All] {
            for fails in [true, false] {
                let mut attempts = 0;
                let (shared, plan) = capture_selected_or_primary(
                    selection,
                    &attached,
                    |plan| {
                        attempts += 1;
                        let shared = CaptureShared::new(4, 2, plan.outputs.len());
                        {
                            let mut state = shared.state.lock().unwrap();
                            if plan.chosen.indices == [1] {
                                state.first_frames_pending = 0;
                            } else if fails {
                                state.error =
                                    Some("first acquisition lost the selected output".into());
                            }
                        }
                        Ok(shared)
                    },
                    |shared| shared.wait_for_first_frames(Duration::ZERO),
                )
                .unwrap();
                assert_eq!(attempts, 2);
                assert_eq!(plan.outputs[0].0, attached[1].name);
                assert_eq!(plan.chosen.bounds, attached[1].rect);
                assert!(plan.fallback_notice().is_some());
                shared.wait_for_first_frames(Duration::ZERO).unwrap();
            }
        }
    }

    #[test]
    fn first_frame_wait_observes_worker_errors_even_after_all_images_arrive() {
        let shared = CaptureShared::new(4, 2, 1);
        thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                shared
                    .wait_for_first_frames(FIRST_FRAME_TIMEOUT)
                    .map_err(|error| error.to_string())
            });
            {
                let mut state = shared.state.lock().unwrap();
                state.first_frames_pending = 0;
                state.error = Some("capture stopped after its first image".into());
            }
            shared.changed.notify_all();
            assert_eq!(
                waiter.join().unwrap().unwrap_err(),
                "capture stopped after its first image"
            );
        });
    }

    #[test]
    fn failed_first_acquisition_releases_capture_before_trying_the_primary() {
        struct CaptureLease<'a> {
            active: &'a AtomicU32,
            primary: bool,
        }
        impl Drop for CaptureLease<'_> {
            fn drop(&mut self) {
                self.active.fetch_sub(1, Ordering::SeqCst);
            }
        }

        let active = AtomicU32::new(0);
        let (capture, plan) = capture_selected_or_primary(
            DisplaySelection::Number(1),
            &recovery_displays(),
            |plan| {
                assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                Ok(CaptureLease {
                    active: &active,
                    primary: plan.chosen.indices == [1],
                })
            },
            |capture| {
                if capture.primary {
                    Ok(())
                } else {
                    Err("first acquisition failed".into())
                }
            },
        )
        .unwrap();
        assert_eq!(plan.chosen.indices, [1]);
        assert_eq!(active.load(Ordering::SeqCst), 1);
        drop(capture);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stale_dxgi_outputs_cannot_replace_the_current_main_display() {
        let layout = vec![recovery_displays()[1].clone()];
        // The disconnected external output still has the old main area in
        // DXGI, including when it has the same size as the replacement.
        assert!(current_output_area("\\\\.\\DISPLAY5", layout[0].rect, &layout).is_none());
        // A surviving output's stale position is not used for input mapping.
        assert!(
            current_output_area("\\\\.\\DISPLAY1", Rect::new(5120, 0, 7680, 1600), &layout)
                .is_none()
        );
        let main = current_output_area("\\\\.\\display1", layout[0].rect, &layout).unwrap();
        assert_eq!(main, &layout[0]);
        assert!(main.primary);
        assert!(current_output_area(&main.name, main.rect, &[]).is_none());
        // Mirroring can put a non-primary output at the origin too.
        let mut mirrored = recovery_displays();
        mirrored[0].rect = mirrored[1].rect;
        assert!(
            !current_output_area(&mirrored[0].name, mirrored[0].rect, &mirrored)
                .unwrap()
                .primary
        );
    }

    #[test]
    fn failed_selected_capture_serves_the_current_primary_and_retains_the_selection() {
        let attached = recovery_displays();
        let mut follower = DisplayFollower::new(DisplaySelection::Number(1));
        let mut attempted = Vec::new();
        let (bounds, plan) = capture_selected_or_primary(
            follower.selection(),
            &attached,
            |plan| {
                attempted.push(plan.outputs.clone());
                if plan.chosen.indices == [0] {
                    Err("external output disconnected".into())
                } else {
                    Ok(plan.chosen.bounds)
                }
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(attempted.len(), 2);
        assert_eq!(attempted[0][0].0, attached[0].name);
        assert_eq!(attempted[1][0].0, attached[1].name);
        assert_eq!(bounds, attached[1].rect);
        assert_eq!(
            plan.announcement(),
            ServedDisplays {
                served: 2,
                count: 2
            }
        );
        assert!(
            plan.fallback_notice()
                .unwrap()
                .contains("external output disconnected")
        );
        follower.started(plan, attached.clone());
        assert_eq!(follower.selection(), DisplaySelection::Number(1));
        for _ in 0..3 {
            assert_eq!(
                follower.layout_read(attached.clone(), || Ok(attached.clone())),
                DisplayChange::None
            );
        }
        // A later topology change retries the requested output.
        let mut returned = attached.clone();
        returned[0].rect = Rect::new(2560, 0, 7680, 1440);
        assert_eq!(
            follower.layout_read(returned.clone(), || Ok(returned.clone())),
            DisplayChange::None
        );
        assert_eq!(
            follower.layout_read(returned.clone(), || Ok(returned.clone())),
            DisplayChange::Restart
        );
        follower.stopped();
        let (_, restored) = capture_selected_or_primary(
            follower.selection(),
            &returned,
            |plan| Ok(plan.chosen.bounds),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(restored.outputs[0].0, attached[0].name);
        assert_eq!(restored.fallback_notice(), None);
    }

    #[test]
    fn all_display_capture_can_fall_back_to_the_primary() {
        let attached = recovery_displays();
        let mut attempted = Vec::new();
        let (_, plan) = capture_selected_or_primary(
            DisplaySelection::All,
            &attached,
            |plan| {
                attempted.push(plan.chosen.indices.clone());
                if plan.outputs.len() > 1 {
                    Err("one output cannot be duplicated".into())
                } else {
                    Ok(())
                }
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(attempted, [vec![0, 1], vec![1]]);
        assert_eq!(plan.chosen.bounds, attached[1].rect);
        assert!(plan.fallback_notice().is_some());
    }

    #[test]
    fn capture_fallback_never_retries_the_same_output_or_an_inaccessible_desktop() {
        let attached = recovery_displays();
        for selection in [
            DisplaySelection::Primary,
            DisplaySelection::Number(2),
            DisplaySelection::Number(9),
        ] {
            let mut attempts = 0;
            let error = capture_selected_or_primary::<()>(
                selection,
                &attached,
                |_| {
                    attempts += 1;
                    Err("main output unavailable".into())
                },
                |_| panic!("failed creation cannot be checked for readiness"),
            )
            .unwrap_err();
            assert_eq!(attempts, 1);
            assert_eq!(error.to_string(), "main output unavailable");
        }
        let mut attempts = 0;
        let error = capture_selected_or_primary::<()>(
            DisplaySelection::Number(1),
            &attached,
            |_| {
                attempts += 1;
                Err(Box::new(CaptureUnavailable {
                    status: CaptureStatus::SecureDesktop,
                    message: "desktop access denied".into(),
                }))
            },
            |_| panic!("an inaccessible desktop cannot be checked for readiness"),
        )
        .unwrap_err();
        assert_eq!(attempts, 1);
        assert_eq!(
            capture_failure_status(error.as_ref()),
            CaptureStatus::SecureDesktop
        );
    }

    #[test]
    fn failed_primary_fallback_preserves_both_errors_and_the_capture_status() {
        let mut attempts = 0;
        let error = capture_selected_or_primary::<()>(
            DisplaySelection::Number(1),
            &recovery_displays(),
            |_| {
                attempts += 1;
                if attempts == 1 {
                    Err("selected output disappeared".into())
                } else {
                    Err(Box::new(CaptureUnavailable {
                        status: CaptureStatus::SecureDesktop,
                        message: "desktop switched during recovery".into(),
                    }))
                }
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(attempts, 2);
        assert!(error.to_string().contains("selected output disappeared"));
        assert!(
            error
                .to_string()
                .contains("desktop switched during recovery")
        );
        assert_eq!(
            capture_failure_status(error.as_ref()),
            CaptureStatus::SecureDesktop
        );
    }

    #[test]
    fn a_successful_capture_does_not_try_another_display() {
        let mut attempts = 0;
        let (_, plan) = capture_selected_or_primary(
            DisplaySelection::Number(1),
            &recovery_displays(),
            |_| {
                attempts += 1;
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(attempts, 1);
        assert_eq!(plan.chosen.indices, [0]);
        assert_eq!(plan.fallback_notice(), None);
        assert!(
            capture_selected_or_primary::<()>(
                DisplaySelection::Primary,
                &[],
                |_| panic!("no display to capture"),
                |_| panic!("no display to check"),
            )
            .is_err()
        );
    }

    #[test]
    fn input_injection_checks_partial_and_silent_windows_rejections() {
        assert!(!input_injection_result(0, 0, 0).unwrap());
        assert!(input_injection_result(2, 2, 0).unwrap());
        assert!(input_injection_result(2, 0, 0).is_err());
        assert!(input_injection_result(2, 1, 0).is_err());
        assert!(
            input_injection_result(2, 0, 5)
                .unwrap_err()
                .to_string()
                .contains("Windows error 5")
        );
    }

    #[test]
    fn console_interrupt_requests_graceful_server_shutdown() {
        SERVER_SHUTDOWN_REQUESTED.store(false, Ordering::Release);
        assert_eq!(unsafe { console_control_handler(u32::MAX) }, 0);
        assert_eq!(unsafe { console_control_handler(CTRL_C_EVENT) }, 1);
        assert!(SERVER_SHUTDOWN_REQUESTED.load(Ordering::Acquire));
        SERVER_SHUTDOWN_REQUESTED.store(false, Ordering::Release);
    }

    #[test]
    fn dxgi_rotations_map_to_capture_rotations() {
        assert_eq!(
            rotation(DXGI_MODE_ROTATION_UNSPECIFIED),
            Ok(Rotation::Identity)
        );
        assert_eq!(
            rotation(DXGI_MODE_ROTATION_ROTATE90),
            Ok(Rotation::Rotate90)
        );
        assert_eq!(
            rotation(DXGI_MODE_ROTATION_ROTATE270),
            Ok(Rotation::Rotate270)
        );
        assert!(rotation(DXGI_MODE_ROTATION(99)).is_err());
    }

    #[test]
    fn button_transitions_press_and_release_only_changed_buttons() {
        let flags = |inputs: Vec<INPUT>| {
            inputs
                .iter()
                .map(|input| unsafe { input.Anonymous.mi.dwFlags })
                .collect::<Vec<_>>()
        };
        assert_eq!(flags(button_inputs(0, 0)), Vec::<u32>::new());
        assert_eq!(
            flags(button_inputs(BUTTON_LEFT, BUTTON_RIGHT)),
            [MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN]
        );
        assert_eq!(
            flags(button_inputs(BUTTON_MIDDLE, BUTTON_MIDDLE)),
            Vec::<u32>::new()
        );
        let inputs = button_inputs(BUTTON_FORWARD, BUTTON_BACK);
        assert_eq!(flags(inputs.clone()), [MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP]);
        let data = inputs
            .iter()
            .map(|input| unsafe { input.Anonymous.mi.mouseData })
            .collect::<Vec<_>>();
        assert_eq!(data, [u32::from(XBUTTON1), u32::from(XBUTTON2)]);
    }

    #[test]
    fn metadata_reads_grow_the_buffer_when_dxgi_needs_more_space() {
        let mut buffer = Vec::<RECT>::new();
        let mut calls = 0;
        let count = read_metadata(&mut buffer, 16, |size, pointer, required| {
            calls += 1;
            let needed = 3 * size_of::<RECT>() as u32;
            *required = needed;
            if size < needed {
                return Err(DXGI_ERROR_MORE_DATA.into());
            }
            unsafe {
                *pointer.add(2) = RECT {
                    left: 1,
                    top: 2,
                    right: 3,
                    bottom: 4,
                }
            };
            Ok(())
        })
        .unwrap();
        assert_eq!((count, calls), (3, 2));
        assert_eq!(buffer[2].right, 3);
    }

    fn sorted(mut displays: Vec<DisplayArea>) -> Vec<DisplayArea> {
        displays.sort_by(|a, b| a.name.cmp(&b.name));
        displays
    }

    fn size(rect: Rect) -> (u16, u16) {
        (
            (rect.right - rect.left) as u16,
            (rect.bottom - rect.top) as u16,
        )
    }

    /// The cheap GDI layout must describe the same displays as DXGI, or
    /// layout changes would be misread.
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn gdi_and_dxgi_agree_on_attached_displays() {
        enable_thread_dpi_awareness();
        let displays = displays();
        assert!(!displays.is_empty());
        assert_eq!(displays.iter().filter(|display| display.primary).count(), 1);
        assert_eq!(sorted(monitor_layout()), sorted(displays));
    }

    /// Every display a monitor shows is reported connected, so filtering by
    /// connection leaves a normally attached desktop unchanged.
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn every_attached_display_has_a_connected_monitor() {
        enable_thread_dpi_awareness();
        let connected = connected_sources().expect("Windows reports display paths");
        assert!(!connected.is_empty());
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.unwrap();
        let mut attached = Vec::new();
        for adapter_index in 0.. {
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(adapter_index) }) else {
                break;
            };
            for output_index in 0.. {
                let Ok(output) = (unsafe { adapter.EnumOutputs(output_index) }) else {
                    break;
                };
                let desc = unsafe { output.GetDesc() }.unwrap();
                if desc.AttachedToDesktop.as_bool() {
                    attached.push(device_name(&desc.DeviceName).to_ascii_uppercase());
                }
            }
        }
        assert!(!attached.is_empty());
        for name in &attached {
            assert!(connected.contains(name), "{name} is not in {connected:?}");
        }
    }

    /// Captures every display alone and all together, and checks that each
    /// display's area of the combined image received pixels.
    #[test]
    #[ignore = "captures the live Windows desktop; run with --test-threads=1"]
    fn captures_each_and_all_live_displays() {
        enable_thread_dpi_awareness();
        let displays = displays();
        let mut selections = vec![DisplaySelection::Primary, DisplaySelection::All];
        selections.extend((1..=displays.len()).map(DisplaySelection::Number));
        for selection in selections {
            let (capture, plan) = start_capture(&mut DisplayFollower::new(selection)).unwrap();
            let chosen = choose_displays(selection, &displays).unwrap();
            assert_eq!(plan.chosen, chosen);
            assert_eq!(
                (capture.placement.width, capture.placement.height),
                size(chosen.bounds),
                "{selection}"
            );
            let mut pixels = vec![
                0;
                usize::from(capture.placement.width)
                    * usize::from(capture.placement.height)
            ];
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(2) {
                capture.next_frame(&mut pixels, &mut Vec::new()).unwrap();
            }
            let stride = usize::from(capture.placement.width);
            for &index in &chosen.indices {
                let area = displays[index]
                    .rect
                    .offset(-chosen.bounds.left, -chosen.bounds.top);
                let colors = (area.top as usize..area.bottom as usize)
                    .step_by(7)
                    .flat_map(|y| {
                        let row = &pixels[y * stride..][..stride];
                        row[area.left as usize..area.right as usize]
                            .iter()
                            .step_by(7)
                            .copied()
                    })
                    .collect::<std::collections::HashSet<_>>();
                // A desktop with a taskbar or wallpaper has many colors; an
                // area never copied stays black.
                assert!(
                    colors.len() > 1,
                    "{selection}: {} received no pixels",
                    displays[index].name
                );
            }
        }
        // With the real attached displays, naming the captured display
        // differently keeps the capture; another display needs a new one.
        let primary = displays.iter().position(|display| display.primary).unwrap();
        let mut follower = DisplayFollower::new(DisplaySelection::Primary);
        let (_capture, _) = start_capture(&mut follower).unwrap();
        assert_eq!(
            follower.request(primary as u8 + 1, attached_areas),
            DisplayChange::None
        );
        assert_eq!(
            follower.plan().unwrap().announcement().served,
            primary as u8 + 1
        );
        if displays.len() > 1 {
            let other = (primary + 1) % displays.len();
            assert_eq!(
                follower.request(other as u8 + 1, attached_areas),
                DisplayChange::Restart
            );
        }
        // An unchanged layout never restarts capture.
        for _ in 0..3 {
            assert_eq!(
                follower.layout_read(monitor_layout(), attached_areas),
                DisplayChange::None
            );
        }
    }

    /// Proof on real hardware, with a person: serves the primary display on
    /// loopback while you change the main display (Settings > System >
    /// Display > Make this my main display) or unplug or plug in a monitor,
    /// then checks that the viewer ends on what is now the primary display.
    /// Waits up to `TOPVNC_MANUAL_SECONDS` (default 120) for the change:
    ///
    /// cargo test --bin topvnc -- --ignored follows_display_changes_made_by_hand --nocapture
    #[test]
    #[ignore = "needs someone to change the main display or unplug a monitor while it runs"]
    fn follows_display_changes_made_by_hand() {
        enable_thread_dpi_awareness();
        let seconds = std::env::var("TOPVNC_MANUAL_SECONDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120);
        // What the viewer must show for a layout: the primary display, its
        // number, and the attached count.
        let expected = |displays: &[DisplayArea]| {
            let primary = displays.iter().position(|display| display.primary)?;
            Some((
                size(displays[primary].rect),
                ServedDisplays {
                    served: primary as u8 + 1,
                    count: displays.len() as u8,
                },
            ))
        };
        let initial = displays();
        let shutdown = AtomicBool::new(false);
        let (notices, receiver) = std::sync::mpsc::channel();
        let notices = Mutex::new(notices);
        let report = |notice: ServerNotice| {
            let _ = notices.lock().unwrap().send(notice);
        };
        let options = ServeOptions {
            tls: topvnc::transport::TlsOptions {
                unencrypted: true,
                ..Default::default()
            },
            address: "127.0.0.1:0".into(),
            allow_insecure: true,
            ..parse_serve_arguments(&[]).unwrap()
        };
        thread::scope(|scope| {
            let host = scope.spawn(|| {
                serve(options, None, &shutdown, &report).map_err(|error| error.to_string())
            });
            let address = loop {
                match receiver.recv_timeout(Duration::from_secs(10)).unwrap() {
                    ServerNotice::Serving { address, .. } => break address,
                    _ => continue,
                }
            };
            let mut session = topvnc::Session::connect_with_encoding(
                &address.to_string(),
                true,
                topvnc::Encoding::Tight { quality: 6 },
                || unreachable!(),
            )
            .unwrap();
            let writer = session.writer();
            // The viewer reads on its own thread, so a quiet desktop that
            // sends nothing cannot stall the checks below.
            let (views, seen) = std::sync::mpsc::channel();
            let viewer = scope.spawn(move || {
                let (width, height) = (session.info.width, session.info.height);
                let mut framebuffer = Framebuffer::new(width, height).unwrap();
                let writer = session.writer();
                let _ = writer.request_update(false, width, height);
                while session
                    .read_update_pipelined(&mut Vec::new(), |x, y, w, h, bytes| {
                        if bytes.is_empty() {
                            framebuffer = Framebuffer::new(w, h)?;
                            Ok(())
                        } else {
                            framebuffer.apply_raw(x, y, w, h, bytes)
                        }
                    })
                    .is_ok()
                {
                    let view = (
                        (session.info.width, session.info.height),
                        writer.serve_display(),
                    );
                    if views.send(view).is_err() {
                        break;
                    }
                }
            });
            eprintln!(
                "\nServing the primary display; {} display(s) attached:",
                initial.len()
            );
            for (index, display) in initial.iter().enumerate() {
                eprintln!(
                    "  {}: {} {:?}{}",
                    index + 1,
                    display.name,
                    size(display.rect),
                    if display.primary { " (main)" } else { "" }
                );
            }
            eprintln!(
                "Within {seconds} s, change the main display or unplug or plug in a monitor.\n"
            );
            let deadline = Instant::now() + Duration::from_secs(seconds);
            let mut latest = None;
            let mut outcome = None;
            // Every layout the test itself reads, so a failure shows whether
            // Windows reported a change at all.
            let mut layout = initial.clone();
            let mut layout_changes = 0;
            while Instant::now() < deadline && outcome.is_none() {
                if let Ok(view) = seen.recv_timeout(Duration::from_millis(500)) {
                    if latest != Some(view) {
                        eprintln!("viewer shows {:?}, host announces {:?}", view.0, view.1);
                    }
                    latest = Some(view);
                }
                while let Ok(notice) = receiver.try_recv() {
                    eprintln!("host: {notice}");
                }
                let now = displays();
                if now != layout {
                    layout_changes += 1;
                    eprintln!("Windows reports a new layout:");
                    for (index, display) in now.iter().enumerate() {
                        eprintln!(
                            "  {}: {} {:?}{}",
                            index + 1,
                            display.name,
                            size(display.rect),
                            if display.primary { " (main)" } else { "" }
                        );
                    }
                    layout = now.clone();
                }
                if now != initial
                    && let (Some(view), Some((size, announced))) = (latest, expected(&now))
                    && view == (size, Some(announced))
                {
                    outcome = Some((now, view));
                }
            }
            shutdown.store(true, Ordering::Release);
            let _ = writer.shutdown();
            viewer.join().unwrap();
            host.join().unwrap().unwrap();
            let (now, view) = outcome.unwrap_or_else(|| {
                assert!(
                    layout_changes > 0,
                    "Windows reported no display change within {seconds} s; change the main \
                     display or unplug a monitor while the test runs"
                );
                let now = displays();
                assert_ne!(
                    now, initial,
                    "the layout changed {layout_changes} time(s) but was back to the original \
                     before the viewer followed; leave the change in place until PASS appears"
                );
                panic!(
                    "Windows reported {layout_changes} layout change(s), but the viewer shows \
                     {latest:?} instead of {:?}",
                    expected(&now)
                )
            });
            eprintln!(
                "\nPASS: after the change the viewer shows {:?}, display {} of {}, the main display.",
                view.0,
                view.1.unwrap().served,
                now.len()
            );
        });
    }

    /// Serves all displays on loopback, then has a viewer switch to each
    /// display and back through the protocol.
    #[test]
    #[ignore = "captures the live Windows desktop; run with --test-threads=1"]
    fn viewers_switch_between_live_displays() {
        enable_thread_dpi_awareness();
        let displays = displays();
        let all = choose_displays(DisplaySelection::All, &displays).unwrap();
        let shutdown = AtomicBool::new(false);
        let (notices, receiver) = std::sync::mpsc::channel();
        let notices = Mutex::new(notices);
        let report = |notice: ServerNotice| {
            let _ = notices.lock().unwrap().send(notice);
        };
        let options = ServeOptions {
            tls: topvnc::transport::TlsOptions {
                unencrypted: true,
                ..Default::default()
            },
            address: "127.0.0.1:0".into(),
            display: DisplaySelection::All,
            allow_insecure: true,
            ..parse_serve_arguments(&[]).unwrap()
        };
        thread::scope(|scope| {
            let host = scope.spawn(|| {
                serve(options, None, &shutdown, &report).map_err(|error| error.to_string())
            });
            let (address, width, height) = loop {
                match receiver.recv_timeout(Duration::from_secs(10)).unwrap() {
                    ServerNotice::Serving {
                        address,
                        width,
                        height,
                        ..
                    } => break (address, width, height),
                    _ => continue,
                }
            };
            assert_eq!((width, height), size(all.bounds));
            let mut session = topvnc::Session::connect_with_encoding(
                &address.to_string(),
                true,
                topvnc::Encoding::Tight { quality: 6 },
                || unreachable!(),
            )
            .unwrap();
            let writer = session.writer();
            let mut framebuffer = Framebuffer::new(width, height).unwrap();
            writer.request_update(false, width, height).unwrap();
            let mut read_until =
                |session: &mut topvnc::Session, done: &dyn Fn(&topvnc::Session) -> bool| {
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while !done(session) {
                        assert!(Instant::now() < deadline, "timed out");
                        session
                            .read_update_pipelined(&mut Vec::new(), |x, y, w, h, bytes| {
                                if bytes.is_empty() {
                                    framebuffer = Framebuffer::new(w, h)?;
                                    Ok(())
                                } else {
                                    framebuffer.apply_raw(x, y, w, h, bytes)
                                }
                            })
                            .unwrap();
                    }
                };
            let count = displays.len() as u8;
            read_until(&mut session, &|_| {
                writer.serve_display() == Some(ServedDisplays { served: 0, count })
            });
            for (index, display) in displays.iter().enumerate() {
                let number = index as u8 + 1;
                writer.request_serve_display(number).unwrap();
                let expected = size(display.rect);
                read_until(&mut session, &|session| {
                    writer.serve_display().map(|displays| displays.served) == Some(number)
                        && (session.info.width, session.info.height) == expected
                });
            }
            writer.request_serve_display(0).unwrap();
            read_until(&mut session, &|session| {
                writer.serve_display().map(|displays| displays.served) == Some(0)
                    && (session.info.width, session.info.height) == (width, height)
            });
            shutdown.store(true, Ordering::Release);
            writer.shutdown().unwrap();
            host.join().unwrap().unwrap();
        });
    }
}
