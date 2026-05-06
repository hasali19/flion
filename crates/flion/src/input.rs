use bitflags::bitflags;
use smol_str::SmolStr;

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
