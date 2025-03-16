use std::collections::HashMap;

use parking_lot::{Mutex, MutexGuard};
use windows::UI::Composition::Visual;

pub struct PlatformViews {
    views: Mutex<HashMap<u64, PlatformView>>,
}

impl PlatformViews {
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

pub struct PlatformView {
    pub visual: Visual,
    // TODO: This probably needs to be Send, possibly Sync
    pub on_update: Box<dyn FnMut(&PlatformViewUpdateArgs)>,
}

#[derive(Debug)]
pub struct PlatformViewUpdateArgs {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
}
