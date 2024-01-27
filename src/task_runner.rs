use std::thread::{self, ThreadId};

use flutter_embedder::{
    FlutterTask, FlutterThreadPriority_kBackground, FlutterThreadPriority_kDisplay,
    FlutterThreadPriority_kRaster,
};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL,
};

pub struct TaskRunner<F> {
    main_thread_id: ThreadId,
    handler: F,
}

impl<F> TaskRunner<F> {
    pub fn new(handler: F) -> TaskRunner<F> {
        TaskRunner {
            main_thread_id: thread::current().id(),
            handler,
        }
    }

    pub fn runs_tasks_on_current_thread(&self) -> bool {
        self.main_thread_id == thread::current().id()
    }
}

impl<F> TaskRunner<F>
where
    F: Fn(u64, FlutterTask),
{
    pub fn post_task(&self, task: flutter_embedder::FlutterTask, target_time_nanos: u64) {
        (self.handler)(target_time_nanos, task);
    }
}

pub unsafe extern "C" fn set_thread_priority(thread_priority: i32) {
    #[expect(non_upper_case_globals)]
    let priority = match thread_priority {
        FlutterThreadPriority_kBackground => THREAD_PRIORITY_BELOW_NORMAL,
        FlutterThreadPriority_kDisplay | FlutterThreadPriority_kRaster => {
            THREAD_PRIORITY_ABOVE_NORMAL
        }
        _ => THREAD_PRIORITY_NORMAL,
    };
    SetThreadPriority(GetCurrentThread(), priority);
}
