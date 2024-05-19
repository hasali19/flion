use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use flutter_embedder::{
    FlutterEngineGetCurrentTime, FlutterTask, FlutterThreadPriority_kBackground,
    FlutterThreadPriority_kDisplay, FlutterThreadPriority_kRaster,
};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL,
};

use crate::engine::FlutterEngine;

#[derive(Debug)]
pub struct Task(u64, FlutterTask);

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
    F: Fn(Task),
{
    pub fn post_task(&self, task: flutter_embedder::FlutterTask, target_time_nanos: u64) {
        (self.handler)(Task(target_time_nanos, task));
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

    if let Err(e) = SetThreadPriority(GetCurrentThread(), priority) {
        tracing::error!("failed to set thread priority: {e}");
    }
}

#[derive(Default)]
pub struct TaskRunnerExecutor {
    tasks: Vec<Task>,
}

impl TaskRunnerExecutor {
    pub fn enqueue(&mut self, task: Task) {
        self.tasks.push(task);
    }

    pub fn process_all(&mut self, engine: &FlutterEngine) -> Option<Instant> {
        let now = unsafe { FlutterEngineGetCurrentTime() };
        let mut next_task_target_time = None;

        self.tasks.retain(|Task(target_time_nanos, task)| {
            if now >= *target_time_nanos {
                engine.run_task(task).unwrap();
                return false;
            }

            let delta = Duration::from_nanos(target_time_nanos - now);
            let target_time = Instant::now() + delta;

            next_task_target_time = Some(if let Some(next) = next_task_target_time {
                std::cmp::min(next, target_time)
            } else {
                target_time
            });

            true
        });

        next_task_target_time
    }
}
