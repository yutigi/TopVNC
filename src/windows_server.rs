//! Windows desktop host for `topvnc --serve` and the Server tab: DXGI Desktop
//! Duplication capture, `SendInput` injection, and clipboard sync.

use crate::desktop_host::{
    CAPTURE_RETRY_MAX, CAPTURE_RETRY_MIN, CaptureSurface, CursorKind, CursorShape, DesktopImage,
    Downscaler, KeyIdentity, MouseMode, PointerModeHint, PointerTransition, Rect, RemoteInputState,
    Rotation, WHEEL_DELTA, absolute_mouse_coordinate, latin1_from_unicode, latin1_to_utf16,
    native_coordinate, served_size, unicode_key_units, validate_capture_dimensions,
    windows_key_identity,
};
use crate::desktop_host::{
    MAX_CLIPBOARD_CHARS, ServeOptions, ServerNotice, VirtualKey, parse_serve_arguments,
};
use std::error::Error;
use std::mem::size_of;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::{Duration, Instant};
use topvnc::{
    BUTTON_BACK, BUTTON_FORWARD, BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, ClientEvent, DamageRect,
    Framebuffer, ServerConfig, VncServer,
};
use windows::Win32::Foundation::{E_ACCESSDENIED, HMODULE, RECT};
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
use windows::core::Interface;
use windows_sys::Win32::Foundation::GlobalFree;
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
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetPriorityClass, HIGH_PRIORITY_CLASS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
    ProcessPowerThrottling, SetPriorityClass, SetProcessInformation,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    SetThreadDpiAwarenessContext,
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
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SetProcessDPIAware, XBUTTON1, XBUTTON2,
};

static SERVER_SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// How long one capture poll waits for a new desktop frame.
const FRAME_TIMEOUT_MS: u32 = 50;
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(1);
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Above this many changed rectangles, copy the whole frame to the CPU once.
const MAX_REGION_COPIES: usize = 64;
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
    let ServeOptions {
        address,
        display,
        allow_insecure,
        scale,
        mouse,
    } = options;
    // The GUI process stays DPI-unaware for its own windows, so the capture
    // and input threads opt in to physical pixels individually.
    enable_thread_dpi_awareness();
    // The game runs in the foreground; keep capture and encoding on pace.
    let _scheduling = HostScheduling::raise(report);
    let mut capture = DesktopCapture::new(display).map_err(|error| {
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
    let mut image = DesktopImage::new(capture.placement.width, capture.placement.height);
    let mut captured = Vec::new();
    let mut damage = Vec::new();
    let started = Instant::now();
    while captured.is_empty() && started.elapsed() < FIRST_FRAME_TIMEOUT {
        capture.next_frame(&mut image, &mut captured)?;
    }
    image.present(&captured, framebuffer.pixels_mut(), &mut damage);
    let native = (capture.placement.width, capture.placement.height);
    let mut served = ServedImage::new(native, scale)?;
    served.update(
        &framebuffer,
        &[Rect::new(0, 0, i32::from(native.0), i32::from(native.1))],
    );
    let (served_width, served_height) = served.size(&framebuffer);
    let config = ServerConfig {
        name: "TopVNC Windows Desktop".into(),
        password,
        allow_insecure,
        // The high priority class already covers every thread.
        thread_setup: None,
    };
    let server = VncServer::bind(&address, served.image(&framebuffer).clone(), config)?;
    report(ServerNotice::Serving {
        address: server.local_addr()?,
        display: capture.name.clone(),
        width: served_width,
        height: served_height,
    });
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
            display,
            scale,
            mouse,
            capture,
            framebuffer,
            image,
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
    display: Option<usize>,
    scale: f32,
    mouse: MouseMode,
    capture: DesktopCapture,
    mut framebuffer: Framebuffer,
    mut image: DesktopImage,
    mut served: ServedImage,
    placement: &Mutex<DisplayPlacement>,
    remote_clipboard_sequence: &AtomicU32,
    shutdown: &AtomicBool,
    report: &(dyn Fn(ServerNotice) + Sync),
) -> Result<(), Box<dyn Error>> {
    let mut captured = Vec::new();
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
    let mut connections = 0;
    let mut pointer_mode = PointerModeHint::default();
    while !shutdown.load(Ordering::Acquire) {
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

        let Some(active) = capture.as_mut() else {
            if Instant::now() < next_retry {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            match DesktopCapture::new(display) {
                Ok(recreated) => {
                    let size = (recreated.placement.width, recreated.placement.height);
                    if size != (framebuffer.width() as u16, framebuffer.height() as u16) {
                        framebuffer = Framebuffer::new(size.0, size.1)?;
                        image = DesktopImage::new(size.0, size.1);
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
                    report(ServerNotice::Message("Desktop capture resumed.".into()));
                    capture = Some(recreated);
                    retry_delay = CAPTURE_RETRY_MIN;
                    last_capture_error.clear();
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
        captured.clear();
        if let Err(error) = active.next_frame(&mut image, &mut captured) {
            // Desktop switches (UAC, lock screen), mode changes, and driver
            // resets all invalidate duplication; keep serving the last image
            // and recreate capture.
            report(ServerNotice::Message(format!(
                "Desktop capture paused: {error}"
            )));
            last_capture_error = error.to_string();
            capture = None;
            next_retry = Instant::now() + retry_delay;
            continue;
        }
        damage.clear();
        image.present(&captured, framebuffer.pixels_mut(), &mut damage);
        if !damage.is_empty() {
            served.update(&framebuffer, &damage);
            if !served.regions.is_empty() {
                server.update_framebuffer_regions(served.image(&framebuffer), &served.regions)?;
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
    while !shutdown.load(Ordering::Acquire) {
        let event = match server.recv_event_timeout(INPUT_POLL_INTERVAL) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match event {
            ClientEvent::Key {
                client_id,
                keysym,
                down,
            } => {
                let key = windows_key_identity(keysym);
                if input.0.key_event(client_id, key, down) {
                    inject_key(key, down);
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
                    inject_pointer(*placement, x, y, transition);
                }
            }
            ClientEvent::RelativePointer {
                client_id,
                buttons,
                dx,
                dy,
            } => {
                let transition = input.0.pointer_event(client_id, buttons);
                inject_relative_pointer(dx, dy, transition);
            }
            ClientEvent::ClientDisconnected { client_id } => {
                let (released, previous, buttons) = input.0.disconnect(client_id);
                for key in released {
                    inject_key(key, false);
                }
                send_inputs(&button_inputs(previous, buttons));
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
    }
}

/// Releases every remotely held key and button when the input thread exits.
struct InputReleaseGuard(RemoteInputState<KeyIdentity>);

impl Drop for InputReleaseGuard {
    fn drop(&mut self) {
        let (keys, buttons) = self.0.release_all();
        for key in keys {
            inject_key(key, false);
        }
        send_inputs(&button_inputs(buttons, 0));
    }
}

fn send_inputs(inputs: &[INPUT]) {
    if !inputs.is_empty() {
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            );
        }
    }
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

fn inject_key(key: KeyIdentity, down: bool) {
    let key_up = if down { 0 } else { KEYEVENTF_KEYUP };
    match key {
        KeyIdentity::Virtual(VirtualKey { code, extended }) => {
            // Scan codes let games that read raw input or DirectInput see the key.
            let scan = unsafe { MapVirtualKeyW(u32::from(code), MAPVK_VK_TO_VSC) } as u16;
            let extended = if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
            send_inputs(&[keyboard_input(code, scan, extended | key_up)]);
        }
        KeyIdentity::Unicode(keysym) => {
            let Some((first, second)) = unicode_key_units(keysym) else {
                return;
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
fn inject_relative_pointer(dx: i32, dy: i32, transition: PointerTransition) {
    let mut inputs = Vec::new();
    if (dx, dy) != (0, 0) {
        inputs.push(mouse_input(dx, dy, 0, MOUSEEVENTF_MOVE));
    }
    inputs.extend(change_inputs(transition));
    send_inputs(&inputs);
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

fn inject_pointer(placement: DisplayPlacement, x: u16, y: u16, transition: PointerTransition) {
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
    send_inputs(&inputs);
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
    let utf16 = latin1_to_utf16(text);
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

/// Desktop Duplication of the primary display. Any failure invalidates the
/// whole capture; the caller recreates it from scratch.
struct DesktopCapture {
    device: ID3D11Device,
    /// Windows raised this device's GPU thread priority, so its copies do
    /// not queue behind a game's rendering.
    gpu_priority_raised: bool,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    placement: DisplayPlacement,
    name: String,
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

impl DesktopCapture {
    fn new(display: Option<usize>) -> Result<Self, Box<dyn Error>> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let (adapter, output) = find_output(&factory, display)?;
        let output_desc = unsafe { output.GetDesc()? };
        let placement = display_placement(&output_desc)?;
        let rotation = rotation(output_desc.Rotation)?;
        let (source_width, source_height) = rotation.source_size(placement.width, placement.height);
        let feature_levels = [
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_10_0,
        ];
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                &adapter,
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
        let output: IDXGIOutput1 = output.cast()?;
        let duplication = unsafe { output.DuplicateOutput(&device) }.map_err(|error| {
            if error.code() == E_ACCESSDENIED {
                format!("Windows denied access to the desktop (secure desktop or locked session): {error}")
            } else {
                format!("could not start Desktop Duplication: {error}")
            }
        })?;
        let name_len = output_desc
            .DeviceName
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(output_desc.DeviceName.len());
        Ok(Self {
            device,
            gpu_priority_raised,
            context,
            duplication,
            staging: None,
            placement,
            name: String::from_utf16_lossy(&output_desc.DeviceName[..name_len]),
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

    /// Wait up to one frame interval for desktop or pointer changes. Changed
    /// desktop regions are copied into `image` and appended to `captured`.
    fn next_frame(
        &mut self,
        image: &mut DesktopImage,
        captured: &mut Vec<Rect>,
    ) -> Result<(), Box<dyn Error>> {
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
        if frame_info.LastMouseUpdateTime != 0 {
            let position = frame_info.PointerPosition.Position;
            image.set_cursor_position(
                position.x,
                position.y,
                frame_info.PointerPosition.Visible.as_bool(),
            );
        }
        if frame_info.PointerShapeBufferSize > 0 {
            image.set_cursor_shape(self.read_pointer_shape(frame_info.PointerShapeBufferSize)?);
        }
        if frame_info.LastPresentTime == 0 && !self.full_frame_pending {
            // Only the pointer changed.
            return Ok(());
        }
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
        if self.source_rects.is_empty() {
            return Ok(());
        }
        let staging = self.staging_texture()?;
        if self.source_rects.len() > MAX_REGION_COPIES {
            unsafe { self.context.CopyResource(&staging, &texture) };
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
                        &texture,
                        0,
                        Some(&region),
                    );
                }
            }
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let _mapped = MappedTexture {
            context: &self.context,
            texture: &staging,
        };
        let (width, height) = (
            usize::from(self.source_width),
            usize::from(self.source_height),
        );
        let row_pitch = mapped.RowPitch as usize;
        if mapped.pData.is_null() || row_pitch < width * 4 {
            return Err("Desktop Duplication returned an invalid row layout".into());
        }
        // SAFETY: the mapped staging texture holds `height` rows of
        // `row_pitch` bytes, the last of which has at least `width` pixels.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                mapped.pData.cast::<u8>(),
                row_pitch * (height - 1) + width * 4,
            )
        };
        let surface = CaptureSurface {
            bytes,
            row_pitch,
            width,
            height,
            rotation: self.rotation,
        };
        for rect in &self.source_rects {
            let desktop = self.rotation.desktop_rect(
                *rect,
                i32::from(self.source_width),
                i32::from(self.source_height),
            );
            image.copy_from(&surface, desktop);
            captured.push(desktop);
        }
        self.full_frame_pending = false;
        Ok(())
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

fn display_placement(desc: &DXGI_OUTPUT_DESC) -> Result<DisplayPlacement, Box<dyn Error>> {
    let area = desc.DesktopCoordinates;
    let width =
        u16::try_from(area.right - area.left).map_err(|_| "display width is outside VNC limits")?;
    let height = u16::try_from(area.bottom - area.top)
        .map_err(|_| "display height is outside VNC limits")?;
    validate_capture_dimensions(width, height)?;
    Ok(DisplayPlacement {
        left: area.left,
        top: area.top,
        width,
        height,
        served_width: width,
        served_height: height,
    })
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

/// The attached output numbered `display` (1-based, in DXGI enumeration
/// order), or else the primary output: the one at desktop origin (0, 0).
fn find_output(
    factory: &IDXGIFactory1,
    display: Option<usize>,
) -> Result<(IDXGIAdapter1, IDXGIOutput), Box<dyn Error>> {
    let mut outputs = Vec::new();
    let mut primary = None;
    for adapter_index in 0..16 {
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
            let desc = unsafe { output.GetDesc()? };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            if desc.DesktopCoordinates.left == 0 && desc.DesktopCoordinates.top == 0 {
                primary.get_or_insert(outputs.len());
            }
            outputs.push((adapter.clone(), output));
        }
    }
    let count = outputs.len();
    let index = match display {
        Some(number) if number <= count => number - 1,
        Some(number) => {
            return Err(
                format!("display {number} not found; {count} display(s) are attached").into(),
            );
        }
        None => primary.unwrap_or(0),
    };
    outputs
        .into_iter()
        .nth(index)
        .ok_or_else(|| "no active display output was found".into())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
