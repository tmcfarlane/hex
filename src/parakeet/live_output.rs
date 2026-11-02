//! Coalesce capture-thread notifications before the single output owner reads them.

use std::collections::BTreeMap;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use crate::dictation::live_text::{self, CaptureId, Subscription, Update};
use crate::paste::Paster;

use super::{OutputJob, WorkerState};

const MAX_CAPTURES: usize = 32;
const MAX_TEXT_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct Pending {
    stable: Option<String>,
    ended: Option<bool>,
}

#[derive(Default)]
struct Queue {
    captures: BTreeMap<CaptureId, Pending>,
    cancel_all: bool,
    wake_queued: bool,
}

impl Queue {
    /// Returns whether a wake should be posted. A full output channel already
    /// contains a job whose receipt drains this inbox, so no waiting is needed.
    fn push(&mut self, update: Update<'_>) -> bool {
        let capture = match update {
            Update::Partial {
                capture,
                partial,
                eligible,
            } => {
                if !eligible || partial.stable.is_empty() {
                    return false;
                }
                capture
            }
            Update::Ended { capture, .. } => capture,
        };
        if !self.captures.contains_key(&capture) && self.captures.len() == MAX_CAPTURES {
            self.captures.clear();
            self.cancel_all = true;
        }
        let pending = self.captures.entry(capture).or_default();
        match update {
            Update::Partial { partial, .. } if pending.ended.is_none() => {
                if partial.stable.len() <= MAX_TEXT_BYTES {
                    pending.stable = Some(partial.stable.clone());
                } else {
                    pending.stable = None;
                    pending.ended = Some(false);
                }
            }
            Update::Ended { submitted, .. } => {
                pending.ended = Some(submitted && pending.ended != Some(false));
                if pending.ended == Some(false) {
                    pending.stable = None;
                }
            }
            Update::Partial { .. } => {}
        }
        let wake = !self.wake_queued;
        self.wake_queued = true;
        wake
    }

    fn take(&mut self) -> (bool, BTreeMap<CaptureId, Pending>) {
        self.wake_queued = false;
        (
            std::mem::take(&mut self.cancel_all),
            std::mem::take(&mut self.captures),
        )
    }
}

#[derive(Default)]
pub(super) struct Inbox(Mutex<Queue>);

impl Inbox {
    pub(super) fn subscribe(output: SyncSender<OutputJob>) -> (Arc<Self>, Subscription) {
        let inbox = Arc::new(Self::default());
        let receiver = inbox.clone();
        let subscription = live_text::watch_owned(move |update| {
            let wake = receiver
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(update);
            if wake {
                let _ = output.try_send(OutputJob::WakeLive);
            }
        });
        (inbox, subscription)
    }

    pub(super) fn pump(&self, paster: &mut Paster, state: &Mutex<WorkerState>) {
        let (cancel_all, pending) = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if cancel_all {
            paster.cancel_all_live();
        }
        for (capture, pending) in pending {
            // A cancelled capture must never briefly insert its last partial.
            if pending.ended == Some(false) {
                paster.live_end(capture, false);
                continue;
            }
            if let Some(stable) = pending.stable {
                let admit = state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .jobs
                    .is_empty();
                paster.live_partial(capture, &stable, admit);
            }
            if let Some(submitted) = pending.ended {
                paster.live_end(capture, submitted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        CompletedTranscript, DictationJobId, JobControl, JobTimings, PasteKind,
        TranscriptionTarget, WorkerEvent, finish_output,
    };
    use super::*;
    use crate::history::{History, HistoryRetention, HistoryStore};
    use crate::transcription::engine::Partial;

    struct HistoryFile(std::path::PathBuf);

    impl HistoryFile {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "hex-live-output-{}-{nonce}.json",
                std::process::id()
            )))
        }
    }

    impl Drop for HistoryFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("json.tmp"));
        }
    }

    fn completed(control: Arc<JobControl>) -> OutputJob {
        OutputJob::Completed {
            job_id: DictationJobId(0),
            capture_id: Some(1),
            control,
            target: TranscriptionTarget::Paste,
            result: Box::new(Ok(CompletedTranscript {
                text: "Final corrected text.".into(),
                raw: "final corrected text".into(),
                application: None,
                processing: None,
                timings: JobTimings {
                    total_started: std::time::Instant::now(),
                    queue_ms: 0,
                    audio_ms: 1000,
                    prepare_ms: 0,
                    inference_ms: 1,
                },
            })),
        }
    }

    fn partial(capture: CaptureId, text: &Partial) -> Update<'_> {
        Update::Partial {
            capture,
            partial: text,
            eligible: true,
        }
    }

    fn text(stable: &str) -> Partial {
        Partial {
            stable: stable.into(),
            tentative: "never insert this".into(),
        }
    }

    #[test]
    fn partials_coalesce_into_one_wake_and_keep_only_stable_text() {
        let mut queue = Queue::default();
        assert!(queue.push(partial(1, &text("one"))));
        assert!(!queue.push(partial(1, &text("one two"))));
        let (cancel_all, mut entries) = queue.take();
        assert!(!cancel_all);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries.remove(&1).unwrap().stable.as_deref(),
            Some("one two")
        );
        assert!(queue.push(partial(1, &text("one two three"))));
    }

    #[test]
    fn cancellation_wins_over_pending_partials_and_later_submission() {
        let mut queue = Queue::default();
        queue.push(partial(1, &text("one")));
        queue.push(Update::Ended {
            capture: 1,
            submitted: false,
        });
        queue.push(partial(1, &text("one two")));
        queue.push(Update::Ended {
            capture: 1,
            submitted: true,
        });
        let (_, mut entries) = queue.take();
        let ended = entries.remove(&1).unwrap();
        assert_eq!(ended.ended, Some(false));
        assert!(ended.stable.is_none());
    }

    #[test]
    fn a_submitted_end_keeps_the_last_partial_before_final_output() {
        let mut queue = Queue::default();
        queue.push(partial(1, &text("one two")));
        queue.push(Update::Ended {
            capture: 1,
            submitted: true,
        });
        let (_, mut entries) = queue.take();
        let ended = entries.remove(&1).unwrap();
        assert_eq!(ended.ended, Some(true));
        assert_eq!(ended.stable.as_deref(), Some("one two"));
    }

    #[test]
    fn ineligible_captures_never_queue_text_or_a_wake() {
        let mut queue = Queue::default();
        assert!(!queue.push(Update::Partial {
            capture: 1,
            partial: &text("one"),
            eligible: false
        }));
        assert!(queue.take().1.is_empty());
    }

    #[test]
    fn overflow_is_bounded_and_retracts_existing_live_drafts() {
        let mut queue = Queue::default();
        for capture in 1..=MAX_CAPTURES as u64 + 1 {
            queue.push(partial(capture, &text("one")));
            assert!(queue.captures.len() <= MAX_CAPTURES);
        }
        let (cancel_all, entries) = queue.take();
        assert!(cancel_all);
        assert_eq!(entries.len(), 1);
        assert!(!queue.take().0);
    }

    #[test]
    fn oversized_text_retracts_instead_of_allocating_an_unbounded_draft() {
        let mut queue = Queue::default();
        queue.push(partial(1, &text(&"x".repeat(MAX_TEXT_BYTES + 1))));
        let (_, mut entries) = queue.take();
        let ended = entries.remove(&1).unwrap();
        assert!(ended.stable.is_none());
        assert_eq!(ended.ended, Some(false));
    }

    #[test]
    fn live_output_failure_keeps_history_and_paste_last_without_retrying() {
        let file = HistoryFile::new();
        let history = History::new(HistoryStore::open(
            file.0.clone(),
            HistoryRetention::Forever,
            0,
        ));
        let mut last = None;
        let mut attempts = 0;
        let event = finish_output(
            completed(Arc::new(JobControl::default())),
            &mut |_, _, _| {
                attempts += 1;
                Err(color_eyre::eyre::eyre!("the field changed"))
            },
            &mut last,
            Some(&history),
        );
        assert!(matches!(
            event,
            WorkerEvent::Completed { result: Err(_), .. }
        ));
        assert_eq!(attempts, 1);
        assert_eq!(last.as_deref(), Some("Final corrected text."));
        let saved = history.search("");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].final_text, "Final corrected text.");
        let event = finish_output(
            OutputJob::Paste {
                sequence: 1,
                kind: PasteKind::LastTranscript,
            },
            &mut |text, _, _| {
                attempts += 1;
                assert_eq!(text, "Final corrected text.");
                Ok(())
            },
            &mut last,
            Some(&history),
        );
        assert!(matches!(event, WorkerEvent::Pasted { result: Ok(_), .. }));
        assert_eq!(attempts, 2);
    }

    #[test]
    fn a_cancelled_live_result_never_outputs_or_becomes_paste_last() {
        let control = Arc::new(JobControl::default());
        control
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        let mut last = None;
        let event = finish_output(
            completed(control),
            &mut |_, _, _| panic!("cancelled output must not paste"),
            &mut last,
            None,
        );
        assert!(matches!(event, WorkerEvent::Cancelled { .. }));
        assert!(last.is_none());
    }
}
