//! Windows keeps shortcuts with the Windows key, such as Win+Space (switch
//! input language), Win+R, and Win+D, for itself: it acts on them before the
//! viewer window sees the keys. While the viewer has focus, a low-level
//! keyboard hook takes the Windows keys, and every key pressed while one is
//! held, before Windows acts on them. It hands them to the event loop, so the
//! whole shortcut reaches the remote computer instead.
//!
//! Windows does not let any app take Win+L or Ctrl+Alt+Del; those still act
//! on this computer.

use std::cell::RefCell;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
    LLKHF_INJECTED, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_USER,
};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::window::KeySender;

/// Virtual-key codes of the left and right Windows keys.
const VK_LWIN: u8 = 0x5b;
const VK_RWIN: u8 = 0x5c;

/// Decides which key events the hook takes from Windows.
///
/// A Windows key pressed while capturing starts a chord. Until the Windows
/// keys are released, every new press is taken too, so Windows never sees a
/// shortcut with a Windows key in it. A release is taken exactly when its
/// press was, so Windows sees every key it saw go down come back up, and no
/// key stays down on this computer.
pub struct Router {
    taken: [bool; 256],
}

impl Default for Router {
    fn default() -> Self {
        Self {
            taken: [false; 256],
        }
    }
}

impl Router {
    /// Whether to take this event from Windows. `capturing` is whether the
    /// viewer may take new presses now.
    pub fn take(&mut self, vk: u8, down: bool, capturing: bool) -> bool {
        let index = usize::from(vk);
        if !down {
            return std::mem::take(&mut self.taken[index]);
        }
        if self.taken[index] {
            // Auto-repeat of a key already taken.
            return true;
        }
        let chord = self.taken[usize::from(VK_LWIN)] || self.taken[usize::from(VK_RWIN)];
        let take = capturing && (vk == VK_LWIN || vk == VK_RWIN || chord);
        self.taken[index] = take;
        take
    }
}

/// The scan code winit would report for this key, with `0xE0` in the high
/// byte for extended keys, or `None` when the keyboard sent no scan code.
fn extended_scancode(scan_code: u32, extended: bool) -> Option<u32> {
    let code = scan_code & 0xff;
    (code != 0).then_some(code | if extended { 0xe000 } else { 0 })
}

/// The physical key, identified the same way as the window's key events.
fn physical_key(event: &KBDLLHOOKSTRUCT) -> PhysicalKey {
    let scancode = extended_scancode(event.scanCode, event.flags & LLKHF_EXTENDED != 0)
        // As winit does for keys, often media keys, that arrive without one.
        .unwrap_or_else(|| unsafe { MapVirtualKeyW(event.vkCode, MAPVK_VK_TO_VSC_EX) });
    PhysicalKey::from_scancode(scancode)
}

/// Where taken keys go: the viewer's event loop.
type KeySink = Box<dyn FnMut(KeyCode, bool) + Send>;

struct HookState {
    /// The viewer window, which must be in front for new keys to be taken.
    window: isize,
    /// The window in front now.
    foreground: fn() -> isize,
    active: Arc<AtomicBool>,
    router: Router,
    keys: KeySink,
}

fn foreground_window() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

impl HookState {
    /// Whether to take the event from Windows; taken keys go to the viewer.
    fn take(&mut self, event: &KBDLLHOOKSTRUCT, message: u32) -> bool {
        let down = match message {
            WM_KEYDOWN | WM_SYSKEYDOWN => true,
            WM_KEYUP | WM_SYSKEYUP => false,
            _ => return false,
        };
        // Keys that programs inject pass through. A TopVNC server on this
        // computer injects the keys its viewers send, and taking them here
        // would loop them back to it.
        if event.flags & LLKHF_INJECTED != 0 {
            return false;
        }
        let Ok(vk) = u8::try_from(event.vkCode) else {
            return false;
        };
        let capturing = self.active.load(Ordering::Relaxed) && (self.foreground)() == self.window;
        if !self.router.take(vk, down, capturing) {
            return false;
        }
        // A key the viewer cannot name is still taken, so Windows never sees
        // half of it; the viewer would not have sent it either.
        if let PhysicalKey::Code(code) = physical_key(event) {
            (self.keys)(code, down);
        }
        true
    }
}

thread_local! {
    /// The hook runs on the thread that installed it, which owns this state.
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, `lparam` points to the event's
        // KBDLLHOOKSTRUCT for the duration of this call.
        let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let taken = STATE.with(|state| {
            state
                .try_borrow_mut()
                .ok()
                .and_then(|mut state| state.as_mut().map(|state| state.take(event, wparam as u32)))
                .unwrap_or(false)
        });
        if taken {
            return 1;
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Takes Windows-key shortcuts for the viewer while it is active and its
/// window is in front. Dropping it removes the hook.
pub struct SystemKeyCapture {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    active: Arc<AtomicBool>,
}

impl SystemKeyCapture {
    /// Install the hook for `window`. It takes nothing until
    /// [`SystemKeyCapture::set_active`] turns it on.
    pub fn start(window: &Window, keys: KeySender) -> io::Result<Self> {
        let window = match window.window_handle().map_err(io::Error::other)?.as_raw() {
            RawWindowHandle::Win32(handle) => handle.hwnd.get(),
            _ => return Err(io::Error::other("the viewer window is not a Win32 window")),
        };
        Self::spawn(window, Box::new(move |code, down| keys.send(code, down)))
    }

    fn spawn(window: isize, keys: KeySink) -> io::Result<Self> {
        let active = Arc::new(AtomicBool::new(false));
        let state = HookState {
            window,
            foreground: foreground_window,
            active: Arc::clone(&active),
            router: Router::default(),
            keys,
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        // The hook gets its own thread. Windows waits for the hook on every
        // key pressed anywhere, so it must never wait behind viewer work.
        let thread = thread::Builder::new()
            .name("topvnc-system-keys".into())
            .spawn(move || run_hook(state, ready_tx))?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                thread: Some(thread),
                active,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(io::Error::other("the keyboard hook thread stopped"))
            }
        }
    }

    /// Whether new presses may be taken: the session wants Windows-key
    /// shortcuts and the viewer has focus. Keys already taken are released
    /// through the viewer either way.
    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }
}

impl Drop for SystemKeyCapture {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_hook(state: HookState, ready: mpsc::SyncSender<io::Result<u32>>) {
    let mut message: MSG = unsafe { std::mem::zeroed() };
    // Create this thread's message queue before anyone posts it WM_QUIT.
    unsafe {
        PeekMessageW(
            &mut message,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        )
    };
    STATE.set(Some(state));
    let hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_hook),
            GetModuleHandleW(std::ptr::null()),
            0,
        )
    };
    if hook.is_null() {
        let _ = ready.send(Err(io::Error::last_os_error()));
        STATE.set(None);
        return;
    }
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    // Windows calls the hook from inside this loop.
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {}
    unsafe { UnhookWindowsHookEx(hook) };
    STATE.set(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Sent = Arc<Mutex<Vec<(KeyCode, bool)>>>;

    const VK_SPACE: u8 = 0x20;
    const VK_SHIFT: u8 = 0xa0;
    const VK_R: u8 = 0x52;

    #[test]
    fn a_windows_key_shortcut_is_taken_whole_and_released_whole() {
        let mut router = Router::default();
        // Win+Space: the Windows key, then Space while it is held.
        assert!(router.take(VK_LWIN, true, true));
        assert!(router.take(VK_LWIN, true, true), "auto-repeat");
        assert!(router.take(VK_SPACE, true, true));
        assert!(router.take(VK_SPACE, false, true));
        assert!(router.take(VK_LWIN, false, true));
        // Space alone is the window's again.
        assert!(!router.take(VK_SPACE, true, true));
        assert!(!router.take(VK_SPACE, false, true));
        // So is the right Windows key's chord.
        assert!(router.take(VK_RWIN, true, true));
        assert!(router.take(VK_R, true, true));
        assert!(router.take(VK_RWIN, false, true));
        assert!(router.take(VK_R, false, true), "released after Win");
    }

    #[test]
    fn keys_pressed_before_the_windows_key_stay_with_windows() {
        let mut router = Router::default();
        assert!(!router.take(VK_SHIFT, true, true));
        assert!(router.take(VK_LWIN, true, true));
        assert!(router.take(VK_R, true, true));
        // Windows saw Shift go down, so it must see it come up.
        assert!(!router.take(VK_SHIFT, false, true));
        assert!(router.take(VK_R, false, true));
        assert!(router.take(VK_LWIN, false, true));
    }

    #[test]
    fn nothing_new_is_taken_without_focus_but_taken_keys_still_come_back() {
        let mut router = Router::default();
        assert!(!router.take(VK_LWIN, true, false));
        assert!(!router.take(VK_LWIN, false, false));

        assert!(router.take(VK_LWIN, true, true));
        // Focus moved while Win was held: new keys belong to Windows, but
        // Windows never saw this Win go down, so its repeat and release
        // stay with the viewer.
        assert!(!router.take(VK_SPACE, true, false));
        assert!(router.take(VK_LWIN, true, false));
        assert!(!router.take(VK_SPACE, false, false));
        assert!(router.take(VK_LWIN, false, false));
        // A release Windows already saw go down is never taken.
        assert!(!router.take(VK_LWIN, false, true));
    }

    #[test]
    fn hook_keys_are_named_like_window_keys() {
        assert_eq!(extended_scancode(0x5b, true), Some(0xe05b));
        assert_eq!(extended_scancode(0x39, false), Some(0x39));
        assert_eq!(extended_scancode(0, true), None);
        let key = |scan_code, flags| {
            physical_key(&KBDLLHOOKSTRUCT {
                vkCode: 0,
                scanCode: scan_code,
                flags,
                time: 0,
                dwExtraInfo: 0,
            })
        };
        assert_eq!(
            key(0x5b, LLKHF_EXTENDED),
            PhysicalKey::Code(KeyCode::SuperLeft)
        );
        assert_eq!(
            key(0x5c, LLKHF_EXTENDED),
            PhysicalKey::Code(KeyCode::SuperRight)
        );
        assert_eq!(key(0x39, 0), PhysicalKey::Code(KeyCode::Space));
        assert_eq!(
            key(0x1d, LLKHF_EXTENDED),
            PhysicalKey::Code(KeyCode::ControlRight)
        );
    }
    const VIEWER: isize = 0x1234;

    fn hook_event(vk: u8, scan_code: u32, flags: u32) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: u32::from(vk),
            scanCode: scan_code,
            flags,
            time: 0,
            dwExtraInfo: 0,
        }
    }

    /// A hook whose viewer window is in front, recording the keys it sends.
    fn hook_state() -> (HookState, Arc<AtomicBool>, Sent) {
        let active = Arc::new(AtomicBool::new(true));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&sent);
        let state = HookState {
            window: VIEWER,
            foreground: || VIEWER,
            active: Arc::clone(&active),
            router: Router::default(),
            keys: Box::new(move |code, down| record.lock().unwrap().push((code, down))),
        };
        (state, active, sent)
    }

    #[test]
    fn the_hook_sends_win_space_to_the_viewer_in_order() {
        let (mut state, _, sent) = hook_state();
        let win = hook_event(VK_LWIN, 0x5b, LLKHF_EXTENDED);
        let space = hook_event(VK_SPACE, 0x39, 0);
        assert!(state.take(&win, WM_KEYDOWN));
        assert!(state.take(&space, WM_KEYDOWN));
        assert!(state.take(&space, WM_KEYUP));
        assert!(state.take(&win, WM_KEYUP));
        assert_eq!(
            *sent.lock().unwrap(),
            [
                (KeyCode::SuperLeft, true),
                (KeyCode::Space, true),
                (KeyCode::Space, false),
                (KeyCode::SuperLeft, false),
            ]
        );
    }

    #[test]
    fn the_hook_leaves_injected_keys_and_other_windows_alone() {
        let (mut state, active, sent) = hook_state();
        // A TopVNC server on this computer injects the keys it is sent.
        let injected = hook_event(VK_LWIN, 0x5b, LLKHF_EXTENDED | LLKHF_INJECTED);
        assert!(!state.take(&injected, WM_KEYDOWN));
        assert!(!state.take(&injected, WM_KEYUP));
        // Not a key message.
        assert!(!state.take(&hook_event(VK_LWIN, 0x5b, LLKHF_EXTENDED), WM_USER));

        let win = hook_event(VK_LWIN, 0x5b, LLKHF_EXTENDED);
        active.store(false, Ordering::Relaxed);
        assert!(!state.take(&win, WM_KEYDOWN));
        assert!(!state.take(&win, WM_KEYUP));
        active.store(true, Ordering::Relaxed);
        state.foreground = || VIEWER + 1;
        assert!(!state.take(&win, WM_SYSKEYDOWN));
        assert!(!state.take(&win, WM_SYSKEYUP));
        assert!(sent.lock().unwrap().is_empty());
    }
    /// Installs a real system-wide hook for a moment. It never takes a key,
    /// because it is never made active.
    #[test]
    #[ignore = "installs a system-wide keyboard hook; needs an interactive desktop"]
    fn the_hook_installs_and_stops_with_its_capture() {
        let started = std::time::Instant::now();
        let capture = SystemKeyCapture::spawn(0, Box::new(|_, _| {})).unwrap();
        capture.set_active(false);
        drop(capture);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
