use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    time::{Duration, Instant},
};

use flutter_embedder::FlutterEngineGetCurrentTime;

use crate::engine::{FlutterEngine, FlutterTask};

pub struct FlutterTaskQueue {
    tasks: BinaryHeap<QueuedTask>,
    next_seq: u64,
}

impl FlutterTaskQueue {
    pub fn new() -> Self {
        FlutterTaskQueue {
            tasks: BinaryHeap::new(),
            next_seq: 0,
        }
    }

    pub fn push(&mut self, task: FlutterTask) {
        self.tasks.push(QueuedTask {
            task,
            seq: self.next_seq,
        });
        self.next_seq += 1;
    }

    pub fn run_tasks(&mut self, engine: &FlutterEngine) {
        let now = unsafe { FlutterEngineGetCurrentTime() };

        while let Some(queued) = self.tasks.peek()
            && queued.task.target_time_nanos() <= now
        {
            let queued = self.tasks.pop().unwrap();
            if let Err(e) = engine.run_task(&queued.task) {
                tracing::error!("{e:?}");
            }
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let queued = self.tasks.peek()?;
        let delay = queued
            .task
            .target_time_nanos()
            .saturating_sub(unsafe { FlutterEngineGetCurrentTime() });

        Some(Instant::now() + Duration::from_nanos(delay))
    }
}

struct QueuedTask {
    task: FlutterTask,
    seq: u64,
}

impl Ord for QueuedTask {
    fn cmp(&self, other: &Self) -> Ordering {
        (other.task.target_time_nanos(), other.seq).cmp(&(self.task.target_time_nanos(), self.seq))
    }
}

impl PartialOrd for QueuedTask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for QueuedTask {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for QueuedTask {}
