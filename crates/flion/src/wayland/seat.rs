use wayland_client::protocol::wl_keyboard::WlKeyboard;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_touch::WlTouch;

use crate::wayland::keyboard::KeyboardState;
use crate::wayland::mouse::MouseState;
use crate::wayland::touch::TouchState;

pub struct Seat {
    pub seat: WlSeat,
    pub pointer: Option<WlPointer>,
    pub mouse: MouseState,
    pub touch: Option<WlTouch>,
    pub touch_state: TouchState,
    pub keyboard: Option<WlKeyboard>,
    pub keyboard_state: KeyboardState,
}

impl Drop for Seat {
    fn drop(&mut self) {
        if let Some(keyboard) = self.keyboard.take() {
            keyboard.release();
        }

        if let Some(touch) = self.touch.take() {
            touch.release();
        }

        if let Some(pointer) = self.pointer.take() {
            pointer.release();
        }

        self.seat.release();
    }
}
