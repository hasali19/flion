use std::sync::{Condvar, Mutex, MutexGuard};

pub struct ResizeController {
    is_resizing: Mutex<bool>,
    condvar: Condvar,
}

impl ResizeController {
    pub fn new() -> ResizeController {
        ResizeController {
            is_resizing: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    pub fn begin_and_wait<T>(&self, block: impl FnOnce() -> T) -> T {
        let mut is_resizing = self.is_resizing.lock().unwrap();

        *is_resizing = true;

        let res = block();

        let _unused = self
            .condvar
            .wait_while(is_resizing, |is_resizing| *is_resizing)
            .unwrap();

        res
    }

    pub fn current_resize(&self) -> Option<ResizeState> {
        let value = self.is_resizing.lock().unwrap();
        if *value {
            Some(ResizeState {
                value,
                condvar: &self.condvar,
            })
        } else {
            None
        }
    }
}

pub struct ResizeState<'a> {
    value: MutexGuard<'a, bool>,
    condvar: &'a Condvar,
}

impl<'a> ResizeState<'a> {
    pub fn complete(mut self) {
        *self.value = false;
        self.condvar.notify_all();
    }
}
