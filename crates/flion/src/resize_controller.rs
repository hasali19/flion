use std::time::Duration;

use parking_lot::{Condvar, Mutex, MutexGuard};

use crate::task_runner::FlutterTaskExecutor;

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

    pub fn begin_and_wait<T>(
        &self,
        width: u32,
        height: u32,
        platform_executor: &FlutterTaskExecutor,
        block: impl FnOnce() -> T,
    ) -> T {
        *self.resize.lock() = Some((width, height));

        let res = block();

        // The Flutter famework may need to run tasks on the platform executor during the resize,
        // so poll the executor instead of blocking to avoid a deadlock.
        while self.resize.lock().is_some() {
            platform_executor.poll_with_timeout(Duration::from_millis(100));
        }

        res
    }

    pub fn current_resize(&self) -> Option<ResizeState> {
        let value = self.resize.lock();
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

impl ResizeState<'_> {
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn complete(mut self) {
        *self.value = None;
        self.condvar.notify_all();
    }
}
