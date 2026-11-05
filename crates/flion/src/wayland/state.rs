use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc};

use calloop::LoopHandle;
use tracing::debug;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::GlobalList,
    protocol::{
        wl_compositor::{self, WlCompositor},
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager,
        wl_data_offer::{self, WlDataOffer},
        wl_data_source, wl_keyboard, wl_pointer,
        wl_registry::{self, WlRegistry},
        wl_seat::{self, Capability},
        wl_surface::{self, WlSurface},
        wl_touch,
    },
};
use wayland_protocols::{
    wp::{
        cursor_shape::v1::client::{
            wp_cursor_shape_device_v1,
            wp_cursor_shape_manager_v1::{self, WpCursorShapeManagerV1},
        },
        fractional_scale::v1::client::{
            wp_fractional_scale_manager_v1::{self, WpFractionalScaleManagerV1},
            wp_fractional_scale_v1,
        },
        viewporter::client::{
            wp_viewport::{self, WpViewport},
            wp_viewporter::{self, WpViewporter},
        },
    },
    xdg::shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::{self, XdgToplevel},
        xdg_wm_base::{self, XdgWmBase},
    },
};

use crate::{
    compositor::ViewSurfaceRegistry,
    egl::EglDisplay,
    engine::FlutterEngine,
    task_queue::FlutterTaskQueue,
    wayland::{
        clipboard::{Clipboard, OfferData},
        mouse_cursor::MouseCursor,
        seat::Seat,
    },
};

pub type WindowId = i64;

pub struct AppState {
    pub egl: EglDisplay,
    pub task_queue: FlutterTaskQueue,
    pub engine: FlutterEngine,
    pub view_surfaces: Arc<ViewSurfaceRegistry>,
    pub windows: BTreeMap<WindowId, Window>,
    pub loop_handle: LoopHandle<'static, AppState>,
    pub cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    pub mouse_cursor: Rc<RefCell<MouseCursor>>,
    pub clipboard: Rc<RefCell<Clipboard>>,
    pub seat: Seat,
}

impl Drop for AppState {
    fn drop(&mut self) {
        if let Some(cursor_shape_manager) = &self.cursor_shape_manager {
            cursor_shape_manager.destroy();
        }
    }
}

pub struct WindowCreator {
    compositor: WlCompositor,
    xdg_wm_base: XdgWmBase,
    viewporter: WpViewporter,
    fractional_scale_manager: WpFractionalScaleManagerV1,
}

impl WindowCreator {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<AppState>) -> eyre::Result<Self> {
        Ok(WindowCreator {
            compositor: globals.bind(qh, 1..=7, ())?,
            xdg_wm_base: globals.bind(qh, 7..=7, ())?,
            viewporter: globals.bind(qh, 1..=1, ())?,
            fractional_scale_manager: globals.bind(qh, 1..=1, ())?,
        })
    }

    pub fn create_window(self: Rc<Self>, id: WindowId, qh: &QueueHandle<AppState>) -> Window {
        let surface = self.compositor.create_surface(qh, id);
        let xdg_surface = self.xdg_wm_base.get_xdg_surface(&surface, qh, id);
        let toplevel = xdg_surface.get_toplevel(qh, id);
        let viewport = self.viewporter.get_viewport(&surface, qh, id);
        let fractional_scale = self
            .fractional_scale_manager
            .get_fractional_scale(&surface, qh, id);

        surface.commit();

        Window {
            id,
            _creator: self,
            surface,
            xdg_surface,
            toplevel,
            viewport,
            fractional_scale,
            size: (0, 0),
            scale_factor: 1.0,
            close_requested: false,
        }
    }
}

impl Drop for WindowCreator {
    fn drop(&mut self) {
        self.fractional_scale_manager.destroy();
        self.viewporter.destroy();
        self.xdg_wm_base.destroy();

        if self.compositor.version() >= 6 {
            self.compositor.release();
        }
    }
}

pub struct Window {
    pub id: WindowId,
    // WindowCreator must outlive its windows.
    _creator: Rc<WindowCreator>,
    pub surface: WlSurface,
    xdg_surface: XdgSurface,
    toplevel: XdgToplevel,
    viewport: WpViewport,
    fractional_scale: wp_fractional_scale_v1::WpFractionalScaleV1,
    size: (u32, u32),
    scale_factor: f64,
    pub close_requested: bool,
}

impl Drop for Window {
    fn drop(&mut self) {
        self.fractional_scale.destroy();
        self.viewport.destroy();
        self.toplevel.destroy();
        self.xdg_surface.destroy();
        self.surface.destroy();
    }
}

impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &wayland_client::globals::GlobalListContents,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            debug!(name, interface, version);
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_compositor::WlCompositor,
        _event: <wl_compositor::WlCompositor as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        todo!()
    }
}

impl Dispatch<wl_surface::WlSurface, WindowId> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_surface::WlSurface,
        _event: <wl_surface::WlSurface as Proxy>::Event,
        _data: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for AppState {
    fn event(
        _state: &mut Self,
        proxy: &xdg_wm_base::XdgWmBase,
        event: <xdg_wm_base::XdgWmBase as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            proxy.pong(serial)
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, WindowId> for AppState {
    fn event(
        state: &mut Self,
        proxy: &xdg_surface::XdgSurface,
        event: <xdg_surface::XdgSurface as Proxy>::Event,
        window_id: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            let Some(window) = state.windows.get(window_id) else {
                return;
            };

            let (width, height) = window.size;
            let scale_factor = window.scale_factor;

            window.viewport.set_destination(width as i32, height as i32);

            let width = f64::from(width) * scale_factor;
            let height = f64::from(height) * scale_factor;

            state
                .engine
                .send_window_metrics_event(
                    *window_id,
                    width as usize,
                    height as usize,
                    scale_factor,
                )
                .unwrap();

            proxy.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, WindowId> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &xdg_toplevel::XdgToplevel,
        event: <xdg_toplevel::XdgToplevel as Proxy>::Event,
        window_id: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        let Some(window) = state.windows.get_mut(window_id) else {
            return;
        };

        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                let width = if width == 0 { 1280 } else { width };
                let height = if height == 0 { 800 } else { height };
                window.size = (width as u32, height as u32);
            }
            xdg_toplevel::Event::Close => {
                window.close_requested = true;
            }
            xdg_toplevel::Event::ConfigureBounds { .. } => {}
            xdg_toplevel::Event::WmCapabilities { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<wp_viewporter::WpViewporter, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_viewporter::WpViewporter,
        _event: <wp_viewporter::WpViewporter as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_viewport::WpViewport, WindowId> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_viewport::WpViewport,
        _event: <wp_viewport::WpViewport as Proxy>::Event,
        _data: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        _event: <wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, WindowId> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: <wp_fractional_scale_v1::WpFractionalScaleV1 as Proxy>::Event,
        window_id: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(window) = state.windows.get_mut(window_id)
        {
            window.scale_factor = f64::from(scale) / 120.0;
            // TODO: Probably need to send a window metrics event to flutter
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for AppState {
    fn event(
        state: &mut Self,
        _seat: &wl_seat::WlSeat,
        event: <wl_seat::WlSeat as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        qh: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(Capability::Pointer) {
                if state.seat.pointer.is_none() {
                    let pointer = state.seat.seat.get_pointer(qh, ());
                    let shape_device = state
                        .cursor_shape_manager
                        .as_ref()
                        .map(|manager| manager.get_pointer(&pointer, qh, ()));

                    state
                        .mouse_cursor
                        .borrow_mut()
                        .set_pointer(Some(pointer.clone()), shape_device);

                    state.seat.pointer = Some(pointer);
                }
            } else if let Some(pointer) = state.seat.pointer.take() {
                state.mouse_cursor.borrow_mut().set_pointer(None, None);
                pointer.release();
            }

            if capabilities.contains(Capability::Touch) {
                if state.seat.touch.is_none() {
                    state.seat.touch = Some(state.seat.seat.get_touch(qh, ()));
                }
            } else if let Some(touch) = state.seat.touch.take() {
                state.seat.touch_state.cancel_all(&state.engine);
                touch.release();
            }

            if capabilities.contains(Capability::Keyboard) {
                if state.seat.keyboard.is_none() {
                    state.seat.keyboard = Some(state.seat.seat.get_keyboard(qh, ()));
                }
            } else if let Some(keyboard) = state.seat.keyboard.take() {
                keyboard.release();
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wl_pointer::WlPointer,
        event: <wl_pointer::WlPointer as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button { serial, .. } = &event {
            state.clipboard.borrow_mut().set_input_serial(*serial);
        }

        if let wl_pointer::Event::Enter { serial, .. } = &event {
            state.mouse_cursor.borrow_mut().on_enter(*serial);
        } else if let wl_pointer::Event::Leave { .. } = &event {
            state.mouse_cursor.borrow_mut().on_leave();
        }

        state.seat.mouse.handle_event(
            &state.engine,
            |surface| surface.data().copied(),
            |id| state.windows.get(&id).map(|window| window.scale_factor),
            event,
        );
    }
}

impl Dispatch<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
        _event: <wp_cursor_shape_manager_v1::WpCursorShapeManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
        _event: <wp_cursor_shape_device_v1::WpCursorShapeDeviceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wl_touch::WlTouch,
        event: <wl_touch::WlTouch as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_touch::Event::Down { serial, .. } = &event {
            state.clipboard.borrow_mut().set_input_serial(*serial);
        }

        state.seat.touch_state.handle_event(
            &state.engine,
            |surface| surface.data().copied(),
            |id| state.windows.get(&id).map(|window| window.scale_factor),
            event,
        );
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wl_keyboard::WlKeyboard,
        event: <wl_keyboard::WlKeyboard as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Enter { serial, .. } | wl_keyboard::Event::Key { serial, .. } =
            &event
        {
            state.clipboard.borrow_mut().set_input_serial(*serial);
        }

        state
            .seat
            .keyboard_state
            .handle_event(&state.engine, &state.loop_handle, event);
    }
}

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_data_device_manager::WlDataDeviceManager,
        _event: <wl_data_device_manager::WlDataDeviceManager as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wl_data_device::WlDataDevice,
        event: <wl_data_device::WlDataDevice as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        state.clipboard.borrow_mut().handle_data_device_event(event);
    }

    wayland_client::event_created_child!(AppState, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferData::default()),
    ]);
}

impl Dispatch<wl_data_offer::WlDataOffer, OfferData> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_data_offer::WlDataOffer,
        event: <wl_data_offer::WlDataOffer as Proxy>::Event,
        data: &OfferData,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        data.handle_event(event);
    }
}

impl Dispatch<wl_data_source::WlDataSource, ()> for AppState {
    fn event(
        state: &mut Self,
        proxy: &wl_data_source::WlDataSource,
        event: <wl_data_source::WlDataSource as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        state
            .clipboard
            .borrow_mut()
            .handle_data_source_event(proxy, event);
    }
}
