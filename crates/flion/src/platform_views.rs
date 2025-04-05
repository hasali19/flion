use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};
use windows::UI::Composition::{Compositor, Visual};

use crate::codec::EncodableValue;
use crate::standard_method_channel::StandardMethodHandler;
use crate::{codec, standard_method_channel};

pub struct PlatformViews {
    views: Mutex<HashMap<u64, PlatformView>>,
}

impl PlatformViews {
    #[expect(clippy::new_without_default)]
    pub fn new() -> PlatformViews {
        PlatformViews {
            views: Mutex::new(HashMap::new()),
        }
    }

    pub fn register(&self, id: u64, view: PlatformView) {
        self.views.lock().insert(id, view);
    }

    pub fn acquire(&self) -> PlatformViewsGuard {
        PlatformViewsGuard(self.views.lock())
    }
}

pub struct PlatformViewsGuard<'a>(MutexGuard<'a, HashMap<u64, PlatformView>>);

impl PlatformViewsGuard<'_> {
    pub fn get_mut(&mut self, id: u64) -> Option<&mut PlatformView> {
        self.0.get_mut(&id)
    }
}

pub type PlatformViewUpdateCallback =
    Box<dyn FnMut(&PlatformViewUpdateArgs) -> eyre::Result<()> + Send + Sync>;

pub struct PlatformView {
    pub visual: Visual,
    pub on_update: PlatformViewUpdateCallback,
}

#[derive(Clone, Debug)]
pub struct PlatformViewUpdateArgs {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
}

pub struct PlatformViewsMessageHandler {
    platform_views: Arc<PlatformViews>,
    compositor: Compositor,
    factories: HashMap<String, Box<dyn PlatformViewFactory>>,
}

pub trait PlatformViewFactory {
    fn create(
        &self,
        compositor: &Compositor,
        id: i32,
        args: EncodableValue,
    ) -> eyre::Result<PlatformView>;
}

impl<F: Fn(&Compositor, i32, EncodableValue) -> eyre::Result<PlatformView>> PlatformViewFactory
    for F
{
    fn create(
        &self,
        compositor: &Compositor,
        id: i32,
        args: EncodableValue,
    ) -> eyre::Result<PlatformView> {
        self(compositor, id, args)
    }
}

impl PlatformViewsMessageHandler {
    pub fn new(
        platform_views: Arc<PlatformViews>,
        compositor: Compositor,
        factories: HashMap<String, Box<dyn PlatformViewFactory>>,
    ) -> PlatformViewsMessageHandler {
        PlatformViewsMessageHandler {
            platform_views,
            compositor,
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

            self.platform_views.register(
                id as u64,
                self.factories[type_]
                    .create(&self.compositor, id, create_args)
                    .unwrap(),
            );

            reply.success(&EncodableValue::Null);
        } else if method == "remove" {
            // TODO
            reply.success(&EncodableValue::Null);
        } else {
            reply.not_implemented();
        }
    }
}
