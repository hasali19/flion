use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::ffi::c_void;
use std::mem;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use bitflags::bitflags;
use eyre::bail;
use smol_str::SmolStr;
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY,
    VK_CONTROL, VK_LCONTROL, VK_LSHIFT, VK_RCONTROL, VK_RSHIFT, VK_SHIFT,
};
use windows::Win32::UI::Input::Touch::{
    CloseTouchInputHandle, GetTouchInputInfo, RegisterTouchWindow, HTOUCHINPUT, TOUCHEVENTF_DOWN,
    TOUCHEVENTF_MOVE, TOUCHEVENTF_UP, TOUCHINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetMessageExtraInfo,
    GetWindowLongPtrW, LoadCursorW, PeekMessageW, RegisterClassExW, SetCursor, SetWindowLongPtrW,
    SystemParametersInfoW, CREATESTRUCTW, GWLP_USERDATA, HCURSOR, HTCLIENT, HWND_MESSAGE,
    IDC_ARROW, PM_NOREMOVE, SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WHEEL_DELTA, WM_CHAR, WM_CLOSE, WM_CREATE, WM_DEADCHAR, WM_DPICHANGED_BEFOREPARENT, WM_KEYDOWN,
    WM_KEYFIRST, WM_KEYLAST, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETCURSOR, WM_SIZE, WM_TOUCH, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSEXW,
    WS_CHILD, WS_EX_NOREDIRECTIONBITMAP, WS_VISIBLE, XBUTTON1, XBUTTON2,
};

use crate::error_utils::ResultExt;

pub struct Window {
    hwnd: HWND,
    window_data: Rc<WindowData>,
}

impl Window {
    pub fn new(width: u32, height: u32, handler: Box<dyn WindowHandler>) -> eyre::Result<Window> {
        static IS_WINDOW_CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

        let hinstance = unsafe { mem::transmute::<HMODULE, HINSTANCE>(GetModuleHandleW(None)?) };

        if !IS_WINDOW_CLASS_REGISTERED.swap(true, Ordering::SeqCst) {
            unsafe {
                RegisterClassExW(&WNDCLASSEXW {
                    cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(wnd_proc),
                    lpszClassName: w!("FlionWindow"),
                    hInstance: hinstance,
                    ..Default::default()
                })
            };
        }

        let window_data = Rc::new(WindowData {
            handler,
            size: Cell::new((width, height)),
            scale_factor: Cell::new(1.0),
            is_tracking_mouse_leave: Cell::new(false),
            cursor: Cell::new(Some(unsafe { LoadCursorW(None, IDC_ARROW)? })),
            cursor_position: Cell::new((0.0, 0.0)),
            mouse_buttons: Cell::new(MouseButtons::empty()),
            keyboard: RefCell::new(Keyboard::default()),
        });

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                w!("FlionWindow"),
                w!("Flion Window"),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                width as i32,
                height as i32,
                Some(HWND_MESSAGE),
                None,
                Some(hinstance),
                Some(Rc::into_raw(window_data.clone()).cast()),
            )?
        };

        Ok(Window { hwnd, window_data })
    }

    pub fn window_handle(&self) -> HWND {
        self.hwnd
    }

    pub fn set_cursor(&self, cursor: Option<HCURSOR>) {
        self.window_data.cursor.set(cursor);
        unsafe { SetCursor(cursor) };
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd).expect("Failed to destroy window");
        }
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MouseButtons: u8 {
        const LEFT = 1 << 0;
        const RIGHT = 1 << 1;
        const MIDDLE = 1 << 2;
        const X1 = 1 << 3;
        const X2 = 1 << 4;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Enter,
    Exit,
    Down,
    Up,
    Move,
    Scroll,
}

#[derive(Clone, Debug)]
pub struct MouseEvent {
    pub action: MouseAction,
    pub x: f64,
    pub y: f64,
    pub buttons: MouseButtons,
    pub scroll_delta_x: f64,
    pub scroll_delta_y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchAction {
    Down,
    Up,
    Move,
}

#[derive(Clone, Debug)]
pub struct TouchEvent {
    pub action: TouchAction,
    pub touch_id: u32,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum KeyAction {
    Up = 1,
    Down = 2,
    Repeat = 3,
}

bitflags! {
    #[derive(Clone, Copy, Default, Debug)]
    pub struct KeyModifiers: u32 {
        const SHIFT = 1 << 0;
        const SHIFT_LEFT = 1 << 1;
        const SHIFT_RIGHT = 1 << 2;
        const CONTROL = 1 << 3;
        const CONTROL_LEFT = 1 << 4;
        const CONTROL_RIGHT = 1 << 5;
        const ALT = 1 << 6;
        const ALT_LEFT = 1 << 7;
        const ALT_RIGHT = 1 << 8;
        const WIN_LEFT = 1 << 9;
        const WIN_RIGHT = 1 << 10;
        const CAPS_LOCK = 1 << 11;
        const NUM_LOCK = 1 << 12;
        const SCROLL_LOCK = 1 << 13;
    }
}

#[derive(Clone, Debug)]
pub struct KeyEvent {
    pub action: KeyAction,
    pub logical: Option<u64>,
    pub physical: Option<u64>,
    pub character: Option<SmolStr>,
    pub modifiers: KeyModifiers,
}

pub trait WindowHandler {
    fn on_resize(&self, width: u32, height: u32, scale_factor: f64);

    fn on_mouse_event(&self, event: MouseEvent);

    fn on_touch_event(&self, event: TouchEvent);

    fn on_key_event(&self, event: KeyEvent);

    fn on_close(&self) {}
}

struct WindowData {
    handler: Box<dyn WindowHandler>,
    size: Cell<(u32, u32)>,
    scale_factor: Cell<f64>,
    is_tracking_mouse_leave: Cell<bool>,
    cursor: Cell<Option<HCURSOR>>,
    cursor_position: Cell<(f64, f64)>,
    mouse_buttons: Cell<MouseButtons>,
    keyboard: RefCell<Keyboard>,
}

impl WindowData {
    fn dispatch_resize_event(&self) {
        let (width, height) = self.size.get();
        let scale_factor = self.scale_factor.get();
        self.handler.on_resize(width, height, scale_factor);
    }

    fn track_mouse_leave_event(&self, hwnd: HWND) {
        if !self.is_tracking_mouse_leave.get() {
            let mut event = TRACKMOUSEEVENT {
                cbSize: mem::size_of::<TRACKMOUSEEVENT>() as u32,
                hwndTrack: hwnd,
                dwFlags: TME_LEAVE,
                dwHoverTime: 0,
            };

            unsafe {
                let _ = TrackMouseEvent(&mut event).trace_err();
            }

            self.is_tracking_mouse_leave.set(true);

            self.dispatch_mouse_event(MouseAction::Enter);
        }
    }

    fn dispatch_mouse_event(&self, action: MouseAction) {
        let (x, y) = self.cursor_position.get();
        let buttons = self.mouse_buttons.get();

        self.handler.on_mouse_event(MouseEvent {
            action,
            x,
            y,
            buttons,
            scroll_delta_x: 0.0,
            scroll_delta_y: 0.0,
        });
    }

    fn on_mouse_scroll(&self, hwnd: HWND, dx: f64, dy: f64) -> eyre::Result<()> {
        let mut cursor_pos = POINT::default();
        let mut lines_per_scroll = 3u32;

        unsafe {
            GetCursorPos(&mut cursor_pos)?;
            ScreenToClient(hwnd, &mut cursor_pos).ok()?;
            SystemParametersInfoW(
                SPI_GETWHEELSCROLLLINES,
                0,
                Some(&raw mut lines_per_scroll as *mut c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
            )?;
        }

        self.cursor_position
            .set((cursor_pos.x as f64, cursor_pos.y as f64));

        let scroll_multiplier = f64::from(lines_per_scroll) * 100.0 / 3.0;

        let dx = -dx * scroll_multiplier;
        let dy = -dy * scroll_multiplier;

        self.dispatch_scroll_event(dx, dy);

        Ok(())
    }

    fn dispatch_scroll_event(&self, dx: f64, dy: f64) {
        let (x, y) = self.cursor_position.get();
        let buttons = self.mouse_buttons.get();

        self.handler.on_mouse_event(MouseEvent {
            action: MouseAction::Scroll,
            x,
            y,
            buttons,
            scroll_delta_x: dx,
            scroll_delta_y: dy,
        });
    }
}

impl Drop for WindowData {
    fn drop(&mut self) {
        tracing::info!("dropping window data");
    }
}

const DPI_BASE: f64 = 96.0;

macro_rules! loword {
    ($e:expr) => {
        ($e.0 & 0xffff)
    };
}

macro_rules! hiword {
    ($e:expr) => {
        (($e.0 >> 16) & 0xffff)
    };
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create_info = lparam.0 as *const CREATESTRUCTW;

        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*create_info).lpCreateParams as isize);

            if let Err(e) = RegisterTouchWindow(hwnd, Default::default()) {
                tracing::error!("Failed to register touch window: {e}");
            }
        }

        return LRESULT(1);
    }

    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowData };
    let window_data = unsafe {
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        // Increment the strong count to ensure that the window data will remain valid for the duration of this call.
        // SAFETY: The pointer will be valid as long as the `Window` instance is valid (since the strong count will be
        // at least 1 until `Window` is dropped). While the `Window` is being dropped, it must be true that DestroyWindow
        // has not terminated if the window procedure is called. Thus, the "last" strong reference must still be alive.
        Rc::increment_strong_count(ptr);
        Rc::from_raw(ptr)
    };

    if msg == WM_NCDESTROY {
        unsafe {
            drop(Rc::from_raw(ptr));
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
    }

    match msg {
        WM_CLOSE => {
            window_data.handler.on_close();
            return LRESULT(0);
        }
        WM_CREATE | WM_DPICHANGED_BEFOREPARENT => {
            let dpi = unsafe { GetDpiForWindow(hwnd) };
            window_data.scale_factor.set(dpi as f64 / DPI_BASE);
            window_data.dispatch_resize_event();
            return LRESULT(0);
        }
        WM_SIZE => {
            let width = loword!(lparam) as u32;
            let height = hiword!(lparam) as u32;
            window_data.size.set((width, height));
            window_data.dispatch_resize_event();
            return LRESULT(0);
        }
        WM_SETCURSOR => {
            let hit_test_result = loword!(lparam);
            if hit_test_result as u32 == HTCLIENT {
                unsafe { SetCursor(window_data.cursor.get()) };
                return LRESULT(1);
            }
        }
        WM_MOUSEMOVE if is_mouse_event() => {
            let x = loword!(lparam) as f64;
            let y = hiword!(lparam) as f64;

            window_data.cursor_position.set((x, y));

            window_data.track_mouse_leave_event(hwnd);
            window_data.dispatch_mouse_event(MouseAction::Move);

            return LRESULT(0);
        }
        WM_MOUSELEAVE if is_mouse_event() => {
            window_data.dispatch_mouse_event(MouseAction::Exit);
            window_data.is_tracking_mouse_leave.set(false);
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN if is_mouse_event() => {
            if msg == WM_LBUTTONDOWN {
                unsafe { SetCapture(hwnd) };
            }

            let x = lparam.0 & 0xffff;
            let y = (lparam.0 >> 16) & 0xffff;

            let button = match msg {
                WM_LBUTTONDOWN => MouseButtons::LEFT,
                WM_RBUTTONDOWN => MouseButtons::RIGHT,
                WM_MBUTTONDOWN => MouseButtons::MIDDLE,
                WM_XBUTTONDOWN => match hiword!(wparam) as u16 {
                    XBUTTON1 => MouseButtons::X1,
                    XBUTTON2 => MouseButtons::X2,
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            };

            window_data.cursor_position.set((x as f64, y as f64));

            window_data
                .mouse_buttons
                .set(window_data.mouse_buttons.get() | button);

            window_data.dispatch_mouse_event(MouseAction::Down);

            return LRESULT(0);
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP if is_mouse_event() => {
            if msg == WM_LBUTTONUP {
                unsafe { ReleaseCapture().unwrap() };
            }

            let x = lparam.0 & 0xffff;
            let y = (lparam.0 >> 16) & 0xffff;

            let button = match msg {
                WM_LBUTTONUP => MouseButtons::LEFT,
                WM_RBUTTONUP => MouseButtons::RIGHT,
                WM_MBUTTONUP => MouseButtons::MIDDLE,
                WM_XBUTTONUP => match hiword!(wparam) as u16 {
                    XBUTTON1 => MouseButtons::X1,
                    XBUTTON2 => MouseButtons::X2,
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            };

            window_data.cursor_position.set((x as f64, y as f64));

            window_data
                .mouse_buttons
                .set(window_data.mouse_buttons.get().difference(button));

            window_data.dispatch_mouse_event(MouseAction::Up);

            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = hiword!(wparam) as i16 as f64 / WHEEL_DELTA as f64;
            let _ = window_data.on_mouse_scroll(hwnd, 0.0, delta).trace_err();
            return LRESULT(0);
        }
        WM_MOUSEHWHEEL => {
            let delta = hiword!(wparam) as i16 as f64 / WHEEL_DELTA as f64;
            let _ = window_data.on_mouse_scroll(hwnd, delta, 0.0).trace_err();
            return LRESULT(0);
        }
        WM_TOUCH => {
            let num_points = wparam.0 & 0xffff;
            let touch_input_handle = HTOUCHINPUT(lparam.0 as _);
            let mut touch_points = vec![TOUCHINPUT::default(); num_points];
            if unsafe {
                GetTouchInputInfo(
                    touch_input_handle,
                    &mut touch_points,
                    mem::size_of::<TOUCHINPUT>() as i32,
                )
                .is_ok()
            } {
                for touch in touch_points {
                    let touch_id = touch.dwID;

                    let mut point = POINT {
                        x: touch.x / 100,
                        y: touch.y / 100,
                    };

                    let _ = unsafe { ScreenToClient(hwnd, &mut point) };

                    let x = point.x as f64;
                    let y = point.y as f64;

                    let action = if touch.dwFlags.contains(TOUCHEVENTF_DOWN) {
                        TouchAction::Down
                    } else if touch.dwFlags.contains(TOUCHEVENTF_UP) {
                        TouchAction::Up
                    } else if touch.dwFlags.contains(TOUCHEVENTF_MOVE) {
                        TouchAction::Move
                    } else {
                        return LRESULT(0);
                    };

                    window_data.handler.on_touch_event(TouchEvent {
                        action,
                        touch_id,
                        x,
                        y,
                    });
                }
            }
            unsafe { CloseTouchInputHandle(touch_input_handle).unwrap() };
            return LRESULT(0);
        }
        WM_KEYDOWN | WM_CHAR | WM_DEADCHAR | WM_KEYUP => {
            match window_data.keyboard.borrow_mut().handle_message(
                hwnd,
                msg,
                wparam,
                lparam,
                |event| window_data.handler.on_key_event(event),
            ) {
                Ok(handled) => {
                    if handled {
                        return LRESULT(0);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to handle keyboard event: {e:?}");
                }
            }
        }
        _ => {}
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn is_mouse_event() -> bool {
    let LPARAM(info) = unsafe { GetMessageExtraInfo() };
    info & 0xFFFFFF00 != 0xFF515700
}

#[derive(Clone)]
struct SystemKeyEvent {
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
}

impl SystemKeyEvent {
    pub fn code(&self) -> u64 {
        self.wparam.0 as u64
    }

    pub fn scan_code(&self) -> u8 {
        ((self.lparam.0 >> 16) & 0xff) as u8
    }

    pub fn was_down(&self) -> bool {
        self.lparam.0 & (1 << 30) != 0
    }
}

#[derive(Default)]
struct Keyboard {
    modifiers: KeyModifiers,
    session: VecDeque<SystemKeyEvent>,
    pressed_keys: BTreeSet<u64>,
}

impl Keyboard {
    fn handle_message(
        &mut self,
        hwnd: HWND,
        msg: u32,
        wparam: windows::Win32::Foundation::WPARAM,
        lparam: windows::Win32::Foundation::LPARAM,
        on_event: impl Fn(KeyEvent),
    ) -> eyre::Result<bool> {
        let event = SystemKeyEvent {
            msg,
            wparam,
            lparam,
        };

        self.session.push_back(event.clone());

        let mut event = match msg {
            WM_KEYDOWN => {
                let next_msg = unsafe { peek_next_message(hwnd) };
                if let Some(WM_CHAR) = next_msg {
                    return Ok(true);
                }

                let scan_code = event.scan_code();
                let key_code = event.code();

                KeyEvent {
                    action: if event.was_down() {
                        KeyAction::Repeat
                    } else {
                        KeyAction::Down
                    },
                    character: None,
                    logical: Some(key_code),
                    physical: Some(scan_code as u64),
                    modifiers: KeyModifiers::empty(),
                }
            }
            WM_CHAR => {
                let next_msg = unsafe { peek_next_message(hwnd) };
                if let Some(WM_CHAR) = next_msg {
                    return Ok(true);
                }

                let Some(
                    key_down @ SystemKeyEvent {
                        msg: WM_KEYDOWN, ..
                    },
                ) = self.session.pop_front()
                else {
                    bail!("Got char event without a key down")
                };

                let scan_code = key_down.scan_code();
                let key_code = key_down.code();

                let code_points = self.session.iter().map(|e| e.code() as u16);
                let chars = char::decode_utf16(code_points.clone())
                    .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER));
                let text = SmolStr::from_iter(chars);

                KeyEvent {
                    action: if key_down.was_down() {
                        KeyAction::Repeat
                    } else {
                        KeyAction::Down
                    },
                    character: Some(text),
                    logical: Some(key_code),
                    physical: Some(scan_code as u64),
                    modifiers: KeyModifiers::empty(),
                }
            }
            WM_KEYUP => {
                let scan_code = event.scan_code();
                let key_code = event.code();

                KeyEvent {
                    action: KeyAction::Up,
                    character: None,
                    logical: Some(key_code),
                    physical: Some(scan_code as u64),
                    modifiers: KeyModifiers::empty(),
                }
            }
            _ => return Ok(false),
        };

        if let Some(logical) = event.logical {
            let vk = VIRTUAL_KEY(logical as u16);

            let is_pressed = event.action != KeyAction::Up;

            let modifier = match vk {
                VK_CONTROL => Some(KeyModifiers::CONTROL),
                VK_LCONTROL => Some(KeyModifiers::CONTROL_LEFT),
                VK_RCONTROL => Some(KeyModifiers::CONTROL_RIGHT),
                VK_SHIFT => Some(KeyModifiers::SHIFT),
                VK_LSHIFT => Some(KeyModifiers::SHIFT_LEFT),
                VK_RSHIFT => Some(KeyModifiers::SHIFT_RIGHT),
                _ => None,
            };

            if let Some(modifier) = modifier {
                self.modifiers.set(modifier, is_pressed);
            }
        }

        self.session.clear();

        if let Some(physical) = event.physical {
            if event.action == KeyAction::Up {
                // Ignore an up event if we haven't recorded that the key is down.
                if !self.pressed_keys.remove(&physical) {
                    tracing::debug!("Key {physical} is not currently pressed. Ignoring up event.");
                    return Ok(true);
                }
            } else {
                let inserted = self.pressed_keys.insert(physical);

                // Ignore a down event if we have already recorded that the key is down. Repeats
                // are still processed as normal.
                if event.action == KeyAction::Down && !inserted {
                    tracing::debug!("Key {physical} is already pressed. Ignoring down event.");
                    return Ok(true);
                }
            }
        }

        event.modifiers = self.modifiers;

        on_event(event);

        Ok(true)
    }
}

unsafe fn peek_next_message(hwnd: HWND) -> Option<u32> {
    let mut msg = Default::default();

    unsafe { PeekMessageW(&mut msg, Some(hwnd), WM_KEYFIRST, WM_KEYLAST, PM_NOREMOVE) }
        .as_bool()
        .then_some(msg.message)
}
