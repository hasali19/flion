use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::DirectComposition::{IDCompositionDevice, IDCompositionVisual};

use crate::codec::EncodableValue;
use crate::standard_method_channel::StandardMethodHandler;
use crate::{codec, standard_method_channel};

pub struct PlatformViews {
    views: Mutex<HashMap<u64, Box<dyn PlatformView>>>,
}

impl PlatformViews {
    #[expect(clippy::new_without_default)]
    pub fn new() -> PlatformViews {
        PlatformViews {
            views: Mutex::new(HashMap::new()),
        }
    }

    fn add(&self, id: u64, view: Box<dyn PlatformView>) {
        self.views.lock().insert(id, view);
    }

    fn remove(&self, id: u64) -> Option<Box<dyn PlatformView>> {
        self.views.lock().remove(&id)
    }

    pub fn acquire(&self) -> PlatformViewsGuard {
        PlatformViewsGuard(self.views.lock())
    }
}

pub struct PlatformViewsGuard<'a>(MutexGuard<'a, HashMap<u64, Box<dyn PlatformView>>>);

impl PlatformViewsGuard<'_> {
    pub fn get_mut(&mut self, id: u64) -> Option<&mut dyn PlatformView> {
        match self.0.get_mut(&id) {
            Some(view) => Some(&mut **view),
            None => None,
        }
    }
}

pub trait PlatformView: Send + Sync {
    fn visual(&mut self) -> &IDCompositionVisual;

    fn update(&mut self, args: &PlatformViewUpdateArgs) -> eyre::Result<()> {
        let _ = args;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct PlatformViewUpdateArgs {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone)]
pub struct CompositorContext<'a> {
    pub d3d11_device: &'a ID3D11Device,
    pub composition_device: &'a IDCompositionDevice,
}

unsafe impl Send for CompositorContext<'_> {}
unsafe impl Sync for CompositorContext<'_> {}

pub trait PlatformViewFactory {
    fn create(
        &self,
        context: CompositorContext,
        id: i32,
        args: EncodableValue,
    ) -> eyre::Result<Box<dyn PlatformView>>;
}

impl<F> PlatformViewFactory for F
where
    F: Fn(CompositorContext, i32, EncodableValue) -> eyre::Result<Box<dyn PlatformView>>,
{
    fn create(
        &self,
        context: CompositorContext,
        id: i32,
        args: EncodableValue,
    ) -> eyre::Result<Box<dyn PlatformView>> {
        self(context, id, args)
    }
}

pub struct PlatformViewsMessageHandler {
    platform_views: Arc<PlatformViews>,
    d3d11_device: ID3D11Device,
    composition_device: IDCompositionDevice,
    factories: HashMap<String, Box<dyn PlatformViewFactory>>,
}

impl PlatformViewsMessageHandler {
    pub fn new(
        platform_views: Arc<PlatformViews>,
        d3d11_device: ID3D11Device,
        composition_device: IDCompositionDevice,
        factories: HashMap<String, Box<dyn PlatformViewFactory>>,
    ) -> PlatformViewsMessageHandler {
        PlatformViewsMessageHandler {
            platform_views,
            d3d11_device,
            composition_device,
            factories,
        }
    }
}

impl StandardMethodHandler for PlatformViewsMessageHandler {
    fn handle(
        &self,
        method: &str,
        args: codec::EncodableValue,
        reply: standard_method_channel::StandardMethodReply,
    ) {
        if method == "create" {
            let mut args = args.into_map().unwrap();

            let id = *args
                .get(&EncodableValue::Str("id"))
                .unwrap()
                .as_i32()
                .unwrap();

            let type_ = args
                .get(&EncodableValue::Str("type"))
                .unwrap()
                .as_string()
                .unwrap();

            let create_args = args
                .remove(&EncodableValue::Str("args"))
                .unwrap_or(EncodableValue::Null);

            let context = CompositorContext {
                d3d11_device: &self.d3d11_device,
                composition_device: &self.composition_device,
            };

            // TODO: Return an error instead of unwrapping
            let platform_view = self.factories[type_]
                .create(context, id, create_args)
                .unwrap();

            self.platform_views.add(id as u64, platform_view);

            reply.success(&EncodableValue::Null);
        } else if method == "destroy" {
            let args = args.into_map().unwrap();

            let id = *args
                .get(&EncodableValue::Str("id"))
                .unwrap()
                .as_i32()
                .unwrap();

            self.platform_views.remove(id as u64);

            reply.success(&EncodableValue::Null);
        } else {
            reply.not_implemented();
        }
    }
}
