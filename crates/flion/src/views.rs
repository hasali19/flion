use std::collections::BTreeMap;

use windows::Win32::Graphics::DirectComposition::IDCompositionVisual;

pub struct ViewManager {
    views: BTreeMap<i64, ViewSurface>,
}

struct DCompositionVisual(IDCompositionVisual);

// DirectComposition can be used from multiple threads: https://learn.microsoft.com/en-us/windows/win32/directcomp/basic-concepts#synchronization
unsafe impl Send for DCompositionVisual {}
unsafe impl Sync for DCompositionVisual {}

pub struct ViewSurface {
    width: u32,
    height: u32,
    is_resizing: bool,
    root_visual: DCompositionVisual,
}

impl ViewSurface {
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn begin_resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.is_resizing = true;
    }

    pub fn end_resize(&mut self) {
        self.is_resizing = false;
    }

    pub fn is_resizing(&self) -> bool {
        self.is_resizing
    }

    pub fn root_visual(&self) -> &IDCompositionVisual {
        &self.root_visual.0
    }
}

impl ViewManager {
    pub(crate) fn new() -> ViewManager {
        ViewManager {
            views: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, view_id: i64, visual: IDCompositionVisual) {
        self.views.insert(
            view_id,
            ViewSurface {
                width: 0,
                height: 0,
                is_resizing: false,
                root_visual: DCompositionVisual(visual),
            },
        );
    }

    pub fn get(&self, view_id: i64) -> Option<&ViewSurface> {
        self.views.get(&view_id)
    }

    pub fn get_mut(&mut self, view_id: i64) -> Option<&mut ViewSurface> {
        self.views.get_mut(&view_id)
    }
}
