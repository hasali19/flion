use std::sync::mpsc::{self, RecvError, RecvTimeoutError};
use std::time::Duration;

use flutter_embedder::{
    FlutterEngine, FlutterEngineGetCurrentTime, FlutterEngineRunTask, FlutterTask,
};

pub struct RenderTask(pub u64, pub FlutterTask);

unsafe impl Send for RenderEvent {}

pub enum RenderEvent {
    #[expect(unused)]
    Stop(mpsc::Sender<()>),
    PostTask(RenderTask),
}

pub fn start(engine: FlutterEngine, event_receiver: mpsc::Receiver<RenderEvent>) {
    std::thread::Builder::new()
        .name(String::from("render"))
        .spawn({
            let engine = engine as usize;
            move || render_main(engine as FlutterEngine, event_receiver)
        })
        .unwrap();
}

fn render_main(engine: FlutterEngine, task_receiver: mpsc::Receiver<RenderEvent>) {
    let mut tasks = vec![];
    let mut wait_time = None;
    loop {
        let event = if let Some(next_sleep) = wait_time {
            match task_receiver.recv_timeout(next_sleep) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => panic!("render thread disconnected"),
            }
        } else {
            match task_receiver.recv() {
                Ok(event) => Some(event),
                Err(RecvError) => {
                    panic!("render thread disconnexted")
                }
            }
        };

        if let Some(event) = event {
            match event {
                RenderEvent::Stop(response) => {
                    response.send(()).ok();
                    return;
                }
                RenderEvent::PostTask(RenderTask(time, task)) => {
                    tasks.push((time, task));
                }
            }
        }

        let now = unsafe { FlutterEngineGetCurrentTime() };

        let mut next_wait_time = None;

        tasks.retain(|(target_time_nanos, task)| {
            if now >= *target_time_nanos {
                unsafe { FlutterEngineRunTask(engine as FlutterEngine, task) };
                return false;
            }

            let delta = Duration::from_nanos(now - target_time_nanos);

            next_wait_time = Some(if let Some(wait_time) = next_wait_time {
                std::cmp::min(wait_time, delta)
            } else {
                delta
            });

            true
        });

        wait_time = next_wait_time;
    }
}
