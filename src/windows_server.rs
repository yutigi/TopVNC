use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::mem::size_of;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use topvnc::{
    ClientEvent, Framebuffer, MAX_FRAMEBUFFER_DIMENSION, MAX_FRAMEBUFFER_PIXELS, ServerConfig,
    VncServer,
};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_ROTATE90,
    DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput,
    IDXGIOutput1, IDXGIOutputDuplication,
};
use windows::core::Interface;
use windows_sys::Win32::Foundation::GlobalFree;
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
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, SetCursorPos};

static SERVER_SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
const MAX_HELD_KEYS_PER_CLIENT: usize = 256;

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

pub fn run(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let mut address = "127.0.0.1:5900".to_owned();
    let mut allow_insecure = false;
    let mut address_set = false;
    for argument in arguments {
        match argument.as_str() {
            "--allow-insecure" => allow_insecure = true,
            _ if argument.starts_with('-') || address_set => {
                return Err("usage: topvnc --serve [HOST:PORT] [--allow-insecure]".into());
            }
            _ => {
                address = argument.clone();
                address_set = true;
            }
        }
    }
    let password = if allow_insecure {
        eprintln!("WARNING: RFB clients will be unauthenticated. Bind only to a trusted network.");
        None
    } else {
        let password = rpassword::prompt_password("VNC server password: ")?;
        if password.is_empty() {
            return Err("server password must not be empty".into());
        }
        Some(password)
    };

    let mut capture = DesktopCapture::new()?;
    let mut framebuffer = Framebuffer::new(capture.width, capture.height)?;
    while !capture.copy_to(&mut framebuffer)? {
        thread::sleep(Duration::from_millis(10));
    }
    let config = ServerConfig {
        name: "TopVNC Windows Desktop".into(),
        password,
        allow_insecure,
    };
    let server = Arc::new(VncServer::bind(&address, framebuffer.clone(), config)?);
    eprintln!(
        "Serving desktop on {}. TCP is unencrypted; use a trusted LAN or secure tunnel.",
        server.local_addr()?
    );
    let listener = Arc::clone(&server);
    SERVER_SHUTDOWN_REQUESTED.store(false, Ordering::Release);
    if unsafe { SetConsoleCtrlHandler(Some(console_control_handler), 1) } == 0 {
        return Err("could not register the server shutdown handler".into());
    }
    let listener_thread = thread::Builder::new()
        .name("topvnc-rfb-listener".into())
        .spawn(move || {
            if let Err(error) = listener.run() {
                eprintln!("VNC listener stopped: {error}");
            }
        })?;

    let mut input_state = InputReleaseGuard(RemoteInputState::default());
    let mut clipboard_sequence = unsafe { GetClipboardSequenceNumber() };
    match read_system_clipboard_latin1() {
        Ok(text) => server.set_clipboard_text(&text)?,
        Err(error) => eprintln!("Could not read the initial system clipboard: {error}"),
    }
    while !SERVER_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        let current_sequence = unsafe { GetClipboardSequenceNumber() };
        if current_sequence != clipboard_sequence {
            match read_system_clipboard_latin1() {
                Ok(text) => {
                    clipboard_sequence = current_sequence;
                    if let Err(error) = server.set_clipboard_text(&text) {
                        eprintln!("Could not publish local clipboard text: {error}");
                    }
                }
                Err(error) => eprintln!("Could not read the system clipboard: {error}"),
            }
        }
        if capture.copy_to(&mut framebuffer)? {
            server.update_framebuffer(&framebuffer)?;
        }
        while let Ok(event) = server.try_event() {
            match event {
                ClientEvent::Key {
                    client_id,
                    keysym,
                    down,
                } => {
                    if input_state.key_event(client_id, keysym, down) {
                        inject_key(keysym, down);
                    }
                }
                ClientEvent::Pointer {
                    client_id,
                    buttons: next,
                    x,
                    y,
                } => {
                    unsafe {
                        let _ = SetCursorPos(i32::from(x), i32::from(y));
                    }
                    let (previous, combined, wheel_up, wheel_down) =
                        input_state.pointer_event(client_id, next);
                    inject_buttons(previous, combined);
                    if wheel_up {
                        wheel(120);
                    }
                    if wheel_down {
                        wheel(-120);
                    }
                }
                ClientEvent::ClientDisconnected { client_id } => {
                    let (released, previous, combined) = input_state.disconnect(client_id);
                    for keysym in released {
                        inject_key_identity(keysym, false);
                    }
                    inject_buttons(previous, combined);
                }
                ClientEvent::ClipboardText { text, .. } => {
                    match set_system_clipboard(&text) {
                        Ok(sequence) => clipboard_sequence = sequence,
                        Err(error) => eprintln!("Could not update the system clipboard: {error}"),
                    }
                    if let Err(error) = server.set_clipboard_text(&text) {
                        eprintln!(
                            "Could not notify other VNC clients about clipboard text: {error}"
                        );
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(4));
    }
    server.stop();
    listener_thread
        .join()
        .map_err(|_| "VNC listener thread panicked")?;
    Ok(())
}

fn latin1_to_utf16(text: &[u8]) -> Vec<u16> {
    text.iter()
        .map(|byte| char::from(*byte) as u16)
        .chain(std::iter::once(0))
        .collect()
}

fn latin1_from_unicode(text: &str) -> Vec<u8> {
    text.chars()
        .take(1_048_576)
        .map(|character| u8::try_from(u32::from(character)).unwrap_or(b'?'))
        .collect()
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
        if byte_len > 2 * (1_048_577usize) {
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

#[derive(Default)]
struct RemoteInputState {
    key_owners: HashMap<KeyIdentity, HashSet<u64>>,
    client_buttons: HashMap<u64, u8>,
}

struct InputReleaseGuard(RemoteInputState);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum KeyIdentity {
    VirtualKey(u16),
    Keysym(u32),
}

impl Deref for InputReleaseGuard {
    type Target = RemoteInputState;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for InputReleaseGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for InputReleaseGuard {
    fn drop(&mut self) {
        let (keys, buttons) = self.0.release_all();
        for keysym in keys {
            inject_key_identity(keysym, false);
        }
        inject_buttons(buttons, 0);
    }
}

impl RemoteInputState {
    fn key_event(&mut self, client_id: u64, keysym: u32, down: bool) -> bool {
        let keysym = key_identity(keysym);
        if down {
            let already_held = self
                .key_owners
                .get(&keysym)
                .is_some_and(|owners| owners.contains(&client_id));
            if !already_held
                && self
                    .key_owners
                    .values()
                    .filter(|owners| owners.contains(&client_id))
                    .count()
                    >= MAX_HELD_KEYS_PER_CLIENT
            {
                return false;
            }
            let owners = self.key_owners.entry(keysym).or_default();
            let inserted = owners.insert(client_id);
            !inserted || owners.len() == 1
        } else if let Some(owners) = self.key_owners.get_mut(&keysym) {
            let should_release = owners.remove(&client_id) && owners.is_empty();
            if owners.is_empty() {
                self.key_owners.remove(&keysym);
            }
            should_release
        } else {
            false
        }
    }

    fn pointer_event(&mut self, client_id: u64, buttons: u8) -> (u8, u8, bool, bool) {
        let old_client_buttons = self.client_buttons.insert(client_id, buttons).unwrap_or(0);
        let old_combined = self.combined_buttons_except(client_id, old_client_buttons);
        let combined = self.combined_buttons();
        (
            old_combined,
            combined,
            buttons & 8 != 0 && old_client_buttons & 8 == 0,
            buttons & 16 != 0 && old_client_buttons & 16 == 0,
        )
    }

    fn combined_buttons(&self) -> u8 {
        self.client_buttons
            .values()
            .fold(0, |all, state| all | (state & 7))
    }

    fn combined_buttons_except(&self, client_id: u64, replacement: u8) -> u8 {
        self.client_buttons
            .iter()
            .filter(|(id, _)| **id != client_id)
            .fold(replacement & 7, |all, (_, state)| all | (state & 7))
    }

    fn disconnect(&mut self, client_id: u64) -> (Vec<KeyIdentity>, u8, u8) {
        let previous = self.combined_buttons();
        let mut released = Vec::new();
        self.key_owners.retain(|keysym, owners| {
            if owners.remove(&client_id) && owners.is_empty() {
                released.push(*keysym);
            }
            !owners.is_empty()
        });
        self.client_buttons.remove(&client_id);
        (released, previous, self.combined_buttons())
    }

    fn release_all(&mut self) -> (Vec<KeyIdentity>, u8) {
        let mut keys = self.key_owners.keys().copied().collect::<Vec<_>>();
        keys.sort_unstable();
        let buttons = self.combined_buttons();
        self.key_owners.clear();
        self.client_buttons.clear();
        (keys, buttons)
    }
}

struct DesktopCapture {
    _device: ID3D11Device,
    output: IDXGIOutput1,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    row_bytes: Vec<u8>,
    pointer_shape: Vec<u8>,
    pointer_shape_info: DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    pointer_position: POINT,
    pointer_visible: bool,
    has_frame: bool,
    width: u16,
    height: u16,
    source_width: u16,
    source_height: u16,
    rotation: i32,
}

struct CaptureDimensions {
    width: u16,
    height: u16,
    source_width: u16,
    source_height: u16,
    rotation: i32,
}

impl DesktopCapture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let (adapter, output): (IDXGIAdapter1, IDXGIOutput) = find_primary_output(&factory)?;
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
            .map_err(|error| {
                format!("could not create the primary display capture device: {error}")
            })?;
        }
        let device = device.ok_or("Direct3D did not return a capture device")?;
        let context = context.ok_or("Direct3D did not return a device context")?;
        let output: IDXGIOutput1 = output.cast()?;
        let duplication = unsafe { output.DuplicateOutput(&device) }.map_err(|error| {
            format!("Windows denied access to the primary desktop capture output: {error}")
        })?;
        let CaptureDimensions {
            width,
            height,
            source_width,
            source_height,
            rotation,
        } = duplication_dimensions(&duplication)?;
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: u32::from(source_width),
            Height: u32::from(source_height),
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
        unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging))? };
        let staging = staging.ok_or("Direct3D did not create a staging texture")?;
        Ok(Self {
            _device: device,
            output,
            context,
            duplication,
            staging,
            row_bytes: vec![0; usize::from(width) * 4],
            pointer_shape: Vec::new(),
            pointer_shape_info: DXGI_OUTDUPL_POINTER_SHAPE_INFO::default(),
            pointer_position: POINT::default(),
            pointer_visible: false,
            has_frame: false,
            width,
            height,
            source_width,
            source_height,
            rotation,
        })
    }

    fn copy_to(&mut self, framebuffer: &mut Framebuffer) -> Result<bool, Box<dyn Error>> {
        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        if let Err(error) = unsafe {
            self.duplication.AcquireNextFrame(
                if self.has_frame { 0 } else { 1000 },
                &mut frame_info,
                &mut resource,
            )
        } {
            if error.code() == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(false);
            }
            if error.code() == DXGI_ERROR_ACCESS_LOST {
                self.recreate_duplication()?;
                return Ok(false);
            }
            return Err(format!("could not acquire the primary desktop frame: {error}").into());
        }
        let _frame = FrameGuard(&self.duplication);
        if frame_info.LastMouseUpdateTime != 0 {
            self.pointer_position = frame_info.PointerPosition.Position;
            self.pointer_visible = frame_info.PointerPosition.Visible.as_bool();
        }
        if frame_info.PointerShapeBufferSize > 0 {
            const MAX_POINTER_BYTES: u32 = 16 * 1024 * 1024;
            let requested = frame_info.PointerShapeBufferSize;
            if requested > MAX_POINTER_BYTES {
                return Err("desktop pointer shape exceeds capture limits".into());
            }
            self.pointer_shape.resize(requested as usize, 0);
            let mut required = 0;
            unsafe {
                self.duplication.GetFramePointerShape(
                    requested,
                    self.pointer_shape.as_mut_ptr().cast(),
                    &mut required,
                    &mut self.pointer_shape_info,
                )?;
            }
            if required > requested {
                return Err("desktop pointer shape changed while being read".into());
            }
            self.pointer_shape.truncate(required as usize);
        }
        let resource = resource.ok_or("Desktop Duplication returned no frame resource")?;
        let texture: ID3D11Texture2D = resource.cast()?;
        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut texture_desc) };
        if texture_desc.Width != u32::from(self.source_width)
            || texture_desc.Height != u32::from(self.source_height)
        {
            return Err("primary display capture dimensions changed".into());
        }
        unsafe { self.context.CopyResource(&self.staging, &texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let _mapped = MappedTexture {
            context: &self.context,
            texture: &self.staging,
        };
        if mapped.pData.is_null() || mapped.RowPitch < u32::from(self.source_width) * 4 {
            return Err("Desktop Duplication returned an invalid row layout".into());
        }
        for row in 0..usize::from(self.height) {
            for column in 0..usize::from(self.width) {
                let (source_x, source_y) = source_coordinate(
                    column,
                    row,
                    usize::from(self.source_width),
                    usize::from(self.source_height),
                    self.rotation,
                );
                let source_offset = source_y * mapped.RowPitch as usize + source_x * 4;
                let source_pixel = unsafe {
                    std::slice::from_raw_parts((mapped.pData as *const u8).add(source_offset), 4)
                };
                self.row_bytes[column * 4..column * 4 + 4].copy_from_slice(source_pixel);
            }
            if self.pointer_visible && !self.pointer_shape.is_empty() {
                composite_cursor_row(
                    &mut self.row_bytes,
                    row as i32,
                    self.width,
                    self.pointer_position,
                    &self.pointer_shape,
                    self.pointer_shape_info,
                )?;
            }
            framebuffer.apply_raw(0, row as u16, self.width, 1, &self.row_bytes)?;
        }
        self.has_frame = true;
        Ok(true)
    }
}

impl DesktopCapture {
    fn recreate_duplication(&mut self) -> Result<(), Box<dyn Error>> {
        let duplication =
            unsafe { self.output.DuplicateOutput(&self._device) }.map_err(|error| {
                format!("could not recreate desktop capture after a display switch: {error}")
            })?;
        let dimensions = duplication_dimensions(&duplication)?;
        if dimensions.width != self.width || dimensions.height != self.height {
            return Err("primary display size changed; restart the VNC server".into());
        }
        if dimensions.source_width != self.source_width
            || dimensions.source_height != self.source_height
        {
            return Err(
                "primary display capture dimensions changed; restart the VNC server".into(),
            );
        }
        self.duplication = duplication;
        self.rotation = dimensions.rotation;
        self.pointer_shape.clear();
        self.pointer_shape_info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        self.pointer_position = POINT::default();
        self.pointer_visible = false;
        self.has_frame = false;
        Ok(())
    }
}

fn duplication_dimensions(
    duplication: &IDXGIOutputDuplication,
) -> Result<CaptureDimensions, Box<dyn Error>> {
    let desc = unsafe { duplication.GetDesc() };
    let width = u16::try_from(desc.ModeDesc.Width)
        .map_err(|_| "primary display width is outside VNC limits")?;
    let height = u16::try_from(desc.ModeDesc.Height)
        .map_err(|_| "primary display height is outside VNC limits")?;
    validate_capture_dimensions(width, height)?;
    let rotation = desc.Rotation.0;
    let (source_width, source_height) = source_dimensions_for_rotation(width, height, rotation)?;
    Ok(CaptureDimensions {
        width,
        height,
        source_width,
        source_height,
        rotation,
    })
}

fn source_dimensions_for_rotation(
    width: u16,
    height: u16,
    rotation: i32,
) -> Result<(u16, u16), &'static str> {
    match rotation {
        value if value == DXGI_MODE_ROTATION_IDENTITY.0 => Ok((width, height)),
        value if value == DXGI_MODE_ROTATION_ROTATE180.0 => Ok((width, height)),
        value
            if value == DXGI_MODE_ROTATION_ROTATE90.0
                || value == DXGI_MODE_ROTATION_ROTATE270.0 =>
        {
            Ok((height, width))
        }
        0 => Ok((width, height)),
        _ => Err("unsupported primary display rotation"),
    }
}

fn validate_capture_dimensions(width: u16, height: u16) -> Result<(), &'static str> {
    let pixels = usize::from(width)
        .checked_mul(usize::from(height))
        .ok_or("primary display pixel count overflow")?;
    if width == 0
        || height == 0
        || width > MAX_FRAMEBUFFER_DIMENSION
        || height > MAX_FRAMEBUFFER_DIMENSION
        || pixels > MAX_FRAMEBUFFER_PIXELS
    {
        return Err("primary display dimensions exceed VNC framebuffer limits");
    }
    Ok(())
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

fn source_coordinate(
    x: usize,
    y: usize,
    source_width: usize,
    source_height: usize,
    rotation: i32,
) -> (usize, usize) {
    if rotation == DXGI_MODE_ROTATION_ROTATE90.0 {
        (y, source_height - 1 - x)
    } else if rotation == DXGI_MODE_ROTATION_ROTATE180.0 {
        (source_width - 1 - x, source_height - 1 - y)
    } else if rotation == DXGI_MODE_ROTATION_ROTATE270.0 {
        (source_width - 1 - y, x)
    } else {
        (x, y)
    }
}

fn composite_cursor_row(
    row: &mut [u8],
    row_y: i32,
    framebuffer_width: u16,
    position: POINT,
    shape: &[u8],
    info: DXGI_OUTDUPL_POINTER_SHAPE_INFO,
) -> Result<(), Box<dyn Error>> {
    let (shape_height, bytes_per_pixel) =
        if info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32 {
            if !info.Height.is_multiple_of(2) {
                return Err("invalid monochrome desktop pointer height".into());
            }
            (info.Height / 2, 0usize)
        } else if info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32
            || info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32
        {
            (info.Height, 4)
        } else {
            return Err("unsupported desktop pointer shape".into());
        };
    if info.Width == 0
        || shape_height == 0
        || info.Width > 4096
        || shape_height > 4096
        || row.len() != usize::from(framebuffer_width) * 4
    {
        return Err("invalid desktop pointer dimensions".into());
    }
    let required = usize::try_from(info.Pitch)?
        .checked_mul(usize::try_from(info.Height)?)
        .ok_or("desktop pointer shape size overflow")?;
    if shape.len() < required
        || (bytes_per_pixel > 0 && info.Pitch < info.Width.saturating_mul(4))
        || (bytes_per_pixel == 0 && info.Pitch < info.Width.div_ceil(8))
    {
        return Err("invalid desktop pointer row layout".into());
    }

    let cursor_y = row_y - position.y;
    if cursor_y < 0 || cursor_y >= shape_height as i32 {
        return Ok(());
    }
    let source_row = cursor_y as usize * info.Pitch as usize;
    for cursor_x in 0..info.Width as i32 {
        let screen_x = position.x + cursor_x;
        if screen_x < 0 || screen_x >= i32::from(framebuffer_width) {
            continue;
        }
        let dest = screen_x as usize * 4;
        if bytes_per_pixel == 0 {
            let mask_byte = cursor_x as usize / 8;
            let mask_bit = 0x80 >> (cursor_x % 8);
            let and_mask = shape[source_row + mask_byte] & mask_bit != 0;
            let xor_row = source_row + (info.Height / 2) as usize * info.Pitch as usize;
            let xor_mask = shape[xor_row + mask_byte] & mask_bit != 0;
            for channel in &mut row[dest..dest + 3] {
                *channel = if and_mask { *channel } else { 0 } ^ if xor_mask { 0xff } else { 0 };
            }
            continue;
        }
        let source = source_row + cursor_x as usize * 4;
        match info.Type {
            value if value == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32 => {
                let alpha = u32::from(shape[source + 3]);
                for channel in 0..3 {
                    let foreground = u32::from(shape[source + channel]);
                    let background = u32::from(row[dest + channel]);
                    row[dest + channel] =
                        ((foreground * alpha + background * (255 - alpha) + 127) / 255) as u8;
                }
            }
            value if value == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32 => {
                match shape[source + 3] {
                    0 => row[dest..dest + 3].copy_from_slice(&shape[source..source + 3]),
                    255 => {
                        for channel in 0..3 {
                            row[dest + channel] ^= shape[source + channel];
                        }
                    }
                    _ => return Err("invalid masked-color pointer alpha value".into()),
                }
            }
            _ => return Err("unsupported desktop pointer shape".into()),
        }
    }
    Ok(())
}

struct FrameGuard<'a>(&'a IDXGIOutputDuplication);

impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.ReleaseFrame();
        }
    }
}

fn find_primary_output(
    factory: &IDXGIFactory1,
) -> Result<(IDXGIAdapter1, IDXGIOutput), Box<dyn Error>> {
    let mut first_output = None;
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
            if first_output.is_none() {
                first_output = Some((adapter.clone(), output.clone()));
            }
            if desc.DesktopCoordinates.left == 0 && desc.DesktopCoordinates.top == 0 {
                return Ok((adapter, output));
            }
        }
    }
    first_output.ok_or_else(|| "no active display output was found".into())
}

fn inject_key(keysym: u32, down: bool) {
    let vk = keysym_to_vk(keysym);
    if let Some(vk) = vk {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
                    ..Default::default()
                },
            },
        };
        unsafe {
            SendInput(1, &input, size_of::<INPUT>() as i32);
        }
    } else if let Some((first, second)) = unicode_key_units(keysym) {
        let units = if down {
            [first, second.unwrap_or(0)]
        } else {
            [
                second.unwrap_or(first),
                if second.is_some() { first } else { 0 },
            ]
        };
        let count = if second.is_some() { 2 } else { 1 };
        let inputs: [INPUT; 2] = std::array::from_fn(|index| unicode_input(units[index], !down));
        unsafe {
            SendInput(count, inputs.as_ptr(), size_of::<INPUT>() as i32);
        }
    }
}

fn key_identity(keysym: u32) -> KeyIdentity {
    keysym_to_vk(keysym)
        .map(KeyIdentity::VirtualKey)
        .unwrap_or(KeyIdentity::Keysym(keysym))
}

fn inject_key_identity(key: KeyIdentity, down: bool) {
    match key {
        KeyIdentity::VirtualKey(vk) => inject_virtual_key(vk, down),
        KeyIdentity::Keysym(keysym) => inject_key(keysym, down),
    }
}

fn inject_virtual_key(vk: u16, down: bool) {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
                ..Default::default()
            },
        },
    };
    unsafe {
        SendInput(1, &input, size_of::<INPUT>() as i32);
    }
}

fn unicode_key_units(keysym: u32) -> Option<(u16, Option<u16>)> {
    let codepoint = match keysym {
        0x20..=0xff => keysym,
        0x0100_0000..=0x0110_ffff => keysym & 0x00ff_ffff,
        _ => return None,
    };
    let character = char::from_u32(codepoint)?;
    let mut encoded = [0; 2];
    let units = character.encode_utf16(&mut encoded);
    Some((units[0], units.get(1).copied()))
}

fn unicode_input(unit: u16, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE | if key_up { KEYEVENTF_KEYUP } else { 0 },
                ..Default::default()
            },
        },
    }
}

fn keysym_to_vk(keysym: u32) -> Option<u16> {
    if let Some(vk) = ascii_keysym_to_vk(keysym) {
        return Some(vk);
    }
    Some(match keysym {
        0xff08 => 0x08,
        0xff09 => 0x09,
        0xff0d => 0x0d,
        0xff1b => 0x1b,
        0xffff => 0x2e,
        0xff50 => 0x24,
        0xff51 => 0x25,
        0xff52 => 0x26,
        0xff53 => 0x27,
        0xff54 => 0x28,
        0xff55 => 0x21,
        0xff56 => 0x22,
        0xff57 => 0x23,
        0xff13 => 0x13,
        0xff14 => 0x91,
        0xff61 => 0x2c,
        0xff7f => 0x90,
        0xff63 => 0x2d,
        0xffe1 | 0xffe2 => 0x10,
        0xffe3 | 0xffe4 => 0x11,
        0xffe9 | 0xffea => 0x12,
        0xffeb => 0x5b,
        0xffec => 0x5c,
        0xffe5 => 0x14,
        0xffbe..=0xffd5 => 0x70 + (keysym - 0xffbe) as u16,
        0xff8d => 0x0d,
        0xffaa => 0x6a,
        0xffab => 0x6b,
        0xffac => 0x6c,
        0xffad => 0x6d,
        0xffae => 0x6e,
        0xffaf => 0x6f,
        0xffb0..=0xffb9 => 0x60 + (keysym - 0xffb0) as u16,
        0xffbd => 0x92,
        0xff67 => 0x5d,
        _ => return None,
    })
}

fn ascii_keysym_to_vk(keysym: u32) -> Option<u16> {
    Some(match keysym {
        0x20 => 0x20,
        0x30..=0x39 => keysym as u16,
        0x41..=0x5a => keysym as u16,
        0x61..=0x7a => (keysym as u16) - 0x20,
        0x21 => 0x31,
        0x22 | 0x27 => 0xde,
        0x23 => 0x33,
        0x24 => 0x34,
        0x25 => 0x35,
        0x26 => 0x37,
        0x28 => 0x39,
        0x29 => 0x30,
        0x2a => 0x38,
        0x2b | 0x3d => 0xbb,
        0x2c | 0x3c => 0xbc,
        0x2d | 0x5f => 0xbd,
        0x2e | 0x3e => 0xbe,
        0x2f | 0x3f => 0xbf,
        0x3a | 0x3b => 0xba,
        0x40 => 0x32,
        0x5b | 0x7b => 0xdb,
        0x5c | 0x7c => 0xdc,
        0x5d | 0x7d => 0xdd,
        0x5e => 0x36,
        0x60 | 0x7e => 0xc0,
        _ => return None,
    })
}

fn inject_buttons(previous: u8, next: u8) {
    let flags = [
        (1, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        (2, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        (4, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
    ];
    for (mask, down_flag, up_flag) in flags {
        if previous & mask != next & mask {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dwFlags: if next & mask != 0 { down_flag } else { up_flag },
                        ..Default::default()
                    },
                },
            };
            unsafe {
                SendInput(1, &input, size_of::<INPUT>() as i32);
            }
        }
    }
    if next & 8 != 0 && previous & 8 == 0 {
        wheel(120);
    }
    if next & 16 != 0 && previous & 16 == 0 {
        wheel(-120);
    }
}

fn wheel(delta: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                mouseData: delta as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                ..Default::default()
            },
        },
    };
    unsafe {
        SendInput(1, &input, size_of::<INPUT>() as i32);
    }
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
    fn disconnect_releases_only_that_clients_held_input() {
        let mut state = RemoteInputState::default();
        assert!(state.key_event(10, 0x61, true));
        assert!(!state.key_event(20, 0x61, true));
        assert!(!state.key_event(10, 0x61, false));

        let (previous, combined, _, _) = state.pointer_event(10, 1);
        assert_eq!((previous, combined), (0, 1));
        let (previous, combined, _, _) = state.pointer_event(20, 1);
        assert_eq!((previous, combined), (1, 1));

        let (released, previous, combined) = state.disconnect(10);
        assert!(released.is_empty());
        assert_eq!((previous, combined), (1, 1));

        let (released, previous, combined) = state.disconnect(20);
        assert_eq!(released, vec![key_identity(0x61)]);
        assert_eq!((previous, combined), (1, 0));
    }

    #[test]
    fn held_keys_are_bounded_per_client() {
        let mut state = RemoteInputState::default();
        for index in 0..MAX_HELD_KEYS_PER_CLIENT as u32 {
            let keysym = 0x0100_0400 + index;
            assert!(state.key_event(1, keysym, true));
        }
        assert!(!state.key_event(1, u32::MAX, true));
        assert!(state.key_event(2, u32::MAX - 1, true));
        assert!(state.key_event(1, 0x0100_0400, false));
        assert!(state.key_event(1, u32::MAX, true));
    }

    #[test]
    fn virtual_key_aliases_share_cross_client_ownership() {
        let mut state = RemoteInputState::default();
        assert!(state.key_event(1, 'a' as u32, true));
        assert!(!state.key_event(2, 'A' as u32, true));
        assert!(!state.key_event(1, 'a' as u32, false));
        assert!(state.key_event(2, 'A' as u32, false));

        assert!(state.key_event(1, '1' as u32, true));
        assert!(!state.key_event(2, '!' as u32, true));
        assert!(!state.key_event(1, '1' as u32, false));
        assert!(state.key_event(2, '!' as u32, false));
    }

    #[test]
    fn wheel_events_are_pulses_and_do_not_stick_as_buttons() {
        let mut state = RemoteInputState::default();
        assert_eq!(state.pointer_event(1, 8), (0, 0, true, false));
        assert_eq!(state.pointer_event(1, 8), (0, 0, false, false));
        assert_eq!(state.pointer_event(1, 0), (0, 0, false, false));
        assert_eq!(state.pointer_event(1, 16), (0, 0, false, true));
    }

    #[test]
    fn unicode_keysyms_cover_latin1_and_supplementary_characters() {
        assert_eq!(unicode_key_units(0x61), Some((0x61, None)));
        assert_eq!(unicode_key_units(0xe9), Some((0xe9, None)));
        assert_eq!(unicode_key_units(0x0101_f600), Some((0xd83d, Some(0xde00))));
        assert_eq!(unicode_key_units(0x0111_0000), None);
        assert_eq!(unicode_key_units(0xff08), None);
    }

    #[test]
    fn ascii_and_extended_keysyms_map_to_windows_virtual_keys() {
        assert_eq!(keysym_to_vk('a' as u32), Some(0x41));
        assert_eq!(keysym_to_vk('A' as u32), Some(0x41));
        assert_eq!(keysym_to_vk('!' as u32), Some(0x31));
        assert_eq!(keysym_to_vk(';' as u32), Some(0xba));
        assert_eq!(keysym_to_vk(0xffc9), Some(0x7b));
        assert_eq!(keysym_to_vk(0xffca), Some(0x7c));
        assert_eq!(keysym_to_vk(0xffd5), Some(0x87));
        assert_eq!(keysym_to_vk(0xffb3), Some(0x63));
        assert_eq!(keysym_to_vk(0xe9), None);
    }

    #[test]
    fn clipboard_latin1_is_converted_to_terminated_utf16() {
        assert_eq!(
            latin1_to_utf16(&[b'h', 0xe9, b'!']),
            [b'h' as u16, 0xe9, b'!' as u16, 0]
        );
        assert_eq!(latin1_to_utf16(&[]), [0]);
        assert_eq!(latin1_from_unicode("hé😀"), [b'h', 0xe9, b'?']);
    }

    #[test]
    fn shutdown_drain_releases_every_remote_key_and_button() {
        let mut state = RemoteInputState::default();
        state.key_event(1, 0x61, true);
        state.key_event(2, 0x62, true);
        state.pointer_event(1, 1);
        state.pointer_event(2, 4);
        assert_eq!(
            state.release_all(),
            (vec![key_identity(0x61), key_identity(0x62)], 5)
        );
        assert!(state.key_owners.is_empty());
        assert_eq!(state.combined_buttons(), 0);
        assert_eq!(state.release_all(), (Vec::new(), 0));
    }

    #[test]
    fn desktop_cursor_composites_color_masked_and_monochrome_shapes() {
        let mut row = vec![0, 0, 0, 0];
        composite_cursor_row(
            &mut row,
            0,
            1,
            POINT::default(),
            &[100, 50, 200, 128],
            DXGI_OUTDUPL_POINTER_SHAPE_INFO {
                Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32,
                Width: 1,
                Height: 1,
                Pitch: 4,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(&row[..3], &[50, 25, 100]);

        row = vec![10, 20, 30, 0, 10, 20, 30, 0];
        composite_cursor_row(
            &mut row,
            0,
            2,
            POINT::default(),
            &[1, 2, 3, 0, 0xff, 0x0f, 0x55, 0xff],
            DXGI_OUTDUPL_POINTER_SHAPE_INFO {
                Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32,
                Width: 2,
                Height: 1,
                Pitch: 8,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(&row, &[1, 2, 3, 0, 0xf5, 0x1b, 0x4b, 0]);

        row = vec![1, 2, 3, 0];
        composite_cursor_row(
            &mut row,
            0,
            1,
            POINT::default(),
            &[0x80, 0x80],
            DXGI_OUTDUPL_POINTER_SHAPE_INFO {
                Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32,
                Width: 1,
                Height: 2,
                Pitch: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(&row[..3], &[0xfe, 0xfd, 0xfc]);
    }

    #[test]
    fn display_rotation_maps_capture_pixels_to_upright_framebuffer() {
        let rotate_90 = (0..2)
            .flat_map(|y| {
                (0..3).map(move |x| source_coordinate(x, y, 2, 3, DXGI_MODE_ROTATION_ROTATE90.0))
            })
            .collect::<Vec<_>>();
        assert_eq!(rotate_90, [(0, 2), (0, 1), (0, 0), (1, 2), (1, 1), (1, 0)]);

        let rotate_270 = (0..2)
            .flat_map(|y| {
                (0..3).map(move |x| source_coordinate(x, y, 2, 3, DXGI_MODE_ROTATION_ROTATE270.0))
            })
            .collect::<Vec<_>>();
        assert_eq!(rotate_270, [(1, 0), (1, 1), (1, 2), (0, 0), (0, 1), (0, 2)]);

        assert_eq!(
            source_coordinate(0, 0, 3, 2, DXGI_MODE_ROTATION_ROTATE180.0),
            (2, 1)
        );
    }

    #[test]
    fn capture_dimensions_are_bounded_before_texture_allocation() {
        assert!(validate_capture_dimensions(8192, 4096).is_ok());
        assert!(validate_capture_dimensions(8192, 4097).is_err());
        assert!(validate_capture_dimensions(8193, 1).is_err());
        assert!(validate_capture_dimensions(0, 1).is_err());
    }

    #[test]
    fn capture_recreation_keeps_source_dimensions_consistent_with_rotation() {
        assert_eq!(
            source_dimensions_for_rotation(1920, 1080, DXGI_MODE_ROTATION_IDENTITY.0),
            Ok((1920, 1080))
        );
        assert_eq!(
            source_dimensions_for_rotation(1080, 1920, DXGI_MODE_ROTATION_ROTATE90.0),
            Ok((1920, 1080))
        );
        assert_eq!(
            source_dimensions_for_rotation(1080, 1920, DXGI_MODE_ROTATION_ROTATE270.0),
            Ok((1920, 1080))
        );
        assert!(source_dimensions_for_rotation(1, 1, 99).is_err());
    }
}
