use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use mpr2mpt::batch::{BatchJob, BatchOutcome, convert_batch_with_approvals, preflight_destinations};
use mpr2mpt::overwrite::OverwriteApproval;
use mpr2mpt::{ConversionError, ConversionReport};

pub(super) enum BatchMessage {
    Outcome(BatchOutcome),
    Finished(Option<String>),
}

pub(super) struct RunningBatch {
    pub(super) receiver: Receiver<BatchMessage>,
    worker: Option<JoinHandle<()>>,
}

pub(super) struct ExistingOutput {
    pub(super) path: PathBuf,
    pub(super) approval: Result<OverwriteApproval, ConversionError>,
    index: usize,
}

pub(super) struct PendingOverwrite {
    pub(super) jobs: Vec<BatchJob>,
    pub(super) existing: Vec<ExistingOutput>,
    pub(super) cursor: usize,
}

impl PendingOverwrite {
    pub(super) fn scan(jobs: Vec<BatchJob>) -> Self {
        let preflight = preflight_destinations(&jobs);
        let existing = jobs
            .iter()
            .enumerate()
            .filter(|(index, _)| preflight.get(*index).is_none_or(Option::is_none))
            .filter_map(|(index, job)| {
                let path = job.destination();
                let approval = OverwriteApproval::capture(&path).transpose()?;
                Some(ExistingOutput {
                    index,
                    path,
                    approval,
                })
            })
            .collect();
        Self {
            jobs,
            existing,
            cursor: 0,
        }
    }

    pub(super) fn approvals(self) -> (Vec<BatchJob>, HashMap<usize, OverwriteApproval>) {
        let approvals = self
            .existing
            .into_iter()
            .filter_map(|row| row.approval.ok().map(|approval| (row.index, approval)))
            .collect();
        (self.jobs, approvals)
    }
}

impl RunningBatch {
    pub(super) fn start(
        jobs: Vec<BatchJob>,
        approvals: HashMap<usize, OverwriteApproval>,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("mpr2mpt-batch".into())
            .spawn(move || {
                let finished = convert_batch_with_approvals(&jobs, &approvals, |outcome| {
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
