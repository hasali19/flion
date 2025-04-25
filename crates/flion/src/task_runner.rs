use std::mem;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use eyre::bail;
use flutter_embedder::{
    FlutterEngineGetCurrentTime, FlutterTask, FlutterThreadPriority_kBackground,
    FlutterThreadPriority_kDisplay, FlutterThreadPriority_kRaster,
};
use parking_lot::Mutex;
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, KillTimer, PostMessageW, RegisterClassW, SetTimer, SetWindowLongPtrW,
    TranslateMessage, GWLP_USERDATA, HWND_MESSAGE, MSG, WM_NULL, WM_TIMER, WNDCLASSW,
};

use crate::engine::FlutterEngine;

#[derive(Debug)]
pub struct Task(u64, FlutterTask);

pub struct FlutterTaskRunner<F> {
    main_thread_id: ThreadId,
    handler: F,
}

impl<F> FlutterTaskRunner<F> {
    pub fn new(handler: F) -> FlutterTaskRunner<F> {
        FlutterTaskRunner {
            main_thread_id: thread::current().id(),
            handler,
        }
    }

    pub fn runs_tasks_on_current_thread(&self) -> bool {
        self.main_thread_id == thread::current().id()
    }
}

impl<F> FlutterTaskRunner<F>
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

pub struct FlutterTaskExecutor {
    hwnd: HWND,
    queue: Arc<FlutterTaskQueue>,
}

impl FlutterTaskExecutor {
    pub fn new() -> eyre::Result<FlutterTaskExecutor> {
        static IS_WINDOW_CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

        if !IS_WINDOW_CLASS_REGISTERED.swap(true, Ordering::SeqCst) {
            register_window_class()?;
        }

        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                w!("FlionTaskRunnerWindow"),
                w!(""),
                Default::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(mem::transmute::<HMODULE, HINSTANCE>(GetModuleHandleW(
                    None,
                )?)),
                None,
            )?
        };

        let queue = Arc::new(FlutterTaskQueue {
            hwnd,
            tasks: Mutex::new(Vec::new()),
        });

        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Arc::as_ptr(&queue) as isize);
        }

        Ok(FlutterTaskExecutor { hwnd, queue })
    }

    pub fn init(&self, engine: Rc<FlutterEngine>) {
        let state = Box::into_raw(Box::new(FlutterTaskExecutorState {
            hwnd: self.hwnd,
            engine,
            queue: self.queue.clone(),
        }));

        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, state as isize);
        }
    }

    pub fn queue(&self) -> &Arc<FlutterTaskQueue> {
        &self.queue
    }

    /// Waits until the next task is executed, or `timeout` has elapsed.
    pub fn poll_with_timeout(&self, timeout: Duration) {
        let mut msg = Default::default();
        unsafe {
            // This will post a WM_TIMER to the message queue, so GetMessageW is guaranteed to
            // return after `timeout`.
            SetTimer(Some(self.hwnd), 1, timeout.as_millis() as u32, None);

            if GetMessageW(&mut msg, Some(self.hwnd), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let _ = KillTimer(Some(self.hwnd), 1);
        }
    }
}

impl Drop for FlutterTaskExecutor {
    fn drop(&mut self) {
        unsafe {
            let state =
                GetWindowLongPtrW(self.hwnd, GWLP_USERDATA) as *mut FlutterTaskExecutorState;

            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);

            drop(Box::from_raw(state));

            if let Err(e) = DestroyWindow(self.hwnd) {
                tracing::error!("Failed to destroy window: {e}");
            }
        }
    }
}

struct FlutterTaskExecutorState {
    hwnd: HWND,
    queue: Arc<FlutterTaskQueue>,
    engine: Rc<FlutterEngine>,
}

impl FlutterTaskExecutorState {
    pub fn process_tasks(&mut self) {
        let now = unsafe { FlutterEngineGetCurrentTime() };
        let mut next_task_target_time = None;

        let mut tasks_to_run = Vec::new();

        self.queue
            .tasks
            .lock()
            .retain(|Task(target_time_nanos, task)| {
                if now >= *target_time_nanos {
                    tasks_to_run.push(*task);
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

        for task in tasks_to_run {
            if let Err(e) = self.engine.run_task(&task) {
                tracing::error!("Failed to run flutter task: {e}");
            }
        }

        if let Some(time) = next_task_target_time {
            let delta = time - Instant::now();
            unsafe {
                SetTimer(Some(self.hwnd), 0, (delta.as_millis() + 1) as u32, None);
            }
        }
    }
}

pub struct FlutterTaskQueue {
    hwnd: HWND,
    tasks: Mutex<Vec<Task>>,
}

unsafe impl Send for FlutterTaskQueue {}

unsafe impl Sync for FlutterTaskQueue {}

impl FlutterTaskQueue {
    pub fn enqueue(&self, task: Task) {
        self.tasks.lock().push(task);
        unsafe {
            if let Err(e) = PostMessageW(
                Some(self.hwnd),
                WM_NULL,
                Default::default(),
                Default::default(),
            ) {
                tracing::error!("Failed to post message to main thread: {e}");
            }
        }
    }
}

fn register_window_class() -> eyre::Result<WNDCLASSW> {
    unsafe {
        let window_class = WNDCLASSW {
            lpszClassName: w!("FlionTaskRunnerWindow"),
            hInstance: mem::transmute::<HMODULE, HINSTANCE>(GetModuleHandleW(None)?),
            lpfnWndProc: Some(wnd_proc),
            ..Default::default()
        };

        if RegisterClassW(&window_class) == 0 {
            let error = GetLastError();
            bail!("Failed to register task runner window class: {error:?}");
        }

        Ok(window_class)
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let executor = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FlutterTaskExecutorState;
    let executor = executor.as_mut();

    if let Some(executor) = executor
        && let WM_NULL | WM_TIMER = msg
    {
        executor.process_tasks();
        return LRESULT(0);
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}
