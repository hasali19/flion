use std::sync::{Condvar, Mutex, MutexGuard};

pub struct ResizeController {
    resize: Mutex<Option<(u32, u32)>>,
    condvar: Condvar,
}

impl ResizeController {
    pub fn new() -> ResizeController {
        ResizeController {
            resize: Mutex::new(None),
            condvar: Condvar::new(),
        }
    }

    pub fn begin_and_wait<T>(&self, width: u32, height: u32, block: impl FnOnce() -> T) -> T {
        let mut resize = self.resize.lock().unwrap();

        *resize = Some((width, height));

        let res = block();

        let _unused = self
            .condvar
            .wait_while(resize, |resize| resize.is_some())
            .unwrap();

        res
    }

    pub fn current_resize(&self) -> Option<ResizeState> {
        let value = self.resize.lock().unwrap();
        value.map(|size| ResizeState {
            size,
            value,
            condvar: &self.condvar,
        })
    }
}

pub struct ResizeState<'a> {
    size: (u32, u32),
    value: MutexGuard<'a, Option<(u32, u32)>>,
    condvar: &'a Condvar,
}

impl<'a> ResizeState<'a> {
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn complete(mut self) {
        *self.value = None;
        self.condvar.notify_all();
    }
}
