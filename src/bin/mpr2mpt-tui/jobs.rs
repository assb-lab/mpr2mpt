use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use mpr2mpt::batch::{BatchJob, BatchOutcome, convert_batch};
use mpr2mpt::{ConversionError, ConversionReport};

pub(super) enum BatchMessage {
    Outcome(BatchOutcome),
    Finished(Option<String>),
}

pub(super) struct RunningBatch {
    pub(super) receiver: Receiver<BatchMessage>,
    worker: Option<JoinHandle<()>>,
}

impl RunningBatch {
    pub(super) fn start(jobs: Vec<BatchJob>) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("mpr2mpt-batch".into())
            .spawn(move || {
                let finished = convert_batch(&jobs, |outcome| {
                    let _ = sender.send(BatchMessage::Outcome(outcome));
                });
                let _ = sender.send(BatchMessage::Finished(
                    finished.err().map(|error| error.to_string()),
                ));
            })?;
        Ok(Self {
            receiver,
            worker: Some(worker),
        })
    }
}

impl Drop for RunningBatch {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) struct Report {
    pub(super) inputs: Vec<PathBuf>,
    pub(super) results: Vec<Option<Result<ConversionReport, ConversionError>>>,
    pub(super) cursor: usize,
    pub(super) failure: Option<String>,
}

impl Report {
    pub(super) fn new(inputs: Vec<PathBuf>) -> Self {
        let results = std::iter::repeat_with(|| None).take(inputs.len()).collect();
        Self {
            inputs,
            results,
            cursor: 0,
            failure: None,
        }
    }
}
