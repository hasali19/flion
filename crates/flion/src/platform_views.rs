use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};
use windows::UI::Composition::{Compositor, Visual};

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
    fn visual(&mut self) -> &Visual;

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
    ) -> eyre::Result<Box<dyn PlatformView>>;
}

impl<F> PlatformViewFactory for F
where
    F: Fn(&Compositor, i32, EncodableValue) -> eyre::Result<Box<dyn PlatformView>>,
{
    fn create(
        &self,
        compositor: &Compositor,
        id: i32,
        args: EncodableValue,
    ) -> eyre::Result<Box<dyn PlatformView>> {
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

            self.platform_views.add(
                id as u64,
                self.factories[type_]
                    .create(&self.compositor, id, create_args)
                    .unwrap(),
            );

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
