//! What the dictation being recorded has said so far, for the HUDs (the
//! Labs feature live text) and the first run's practice field, which
//! [`demand`]s it while it shows and reads it with [`current`].
//!
//! The capture loop publishes through its recording's [`Publisher`], which
//! the recording owns: dropping the recording (release, a short tap, a lost
//! microphone) clears the text, so no ending can leave it on screen.
//! A HUD registers a [`watch`]er and is told each change from the capture
//! loop's thread; it must hand the text to its own thread and never wait.
//! Live text is display-only. The explicit Hex buddy opt-in also lets its
//! output worker insert stable words from eligible physical-shortcut captures.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::transcription::engine::Partial;

pub(crate) type CaptureId = u64;

type Watcher = Arc<dyn Fn(CaptureId, Option<&Partial>) + Send + Sync>;
type OwnedWatcher = Arc<dyn Fn(Update<'_>) + Send + Sync>;

#[derive(Clone, Copy)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum Update<'a> {
    Partial {
        capture: CaptureId,
        partial: &'a Partial,
        eligible: bool,
    },
    Ended {
        capture: CaptureId,
        submitted: bool,
    },
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct Subscription(u64);

/// A released capture either reaches the output worker or retracts its live
/// draft when silence detection/admission discards it.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct Ticket {
    capture: CaptureId,
    accepted: bool,
}

#[cfg(any(target_os = "macos", test))]
impl Ticket {
    pub(crate) fn new(capture: CaptureId) -> Self {
        Self {
            capture,
            accepted: false,
        }
    }
    pub(crate) fn id(&self) -> CaptureId {
        self.capture
    }
    pub(crate) fn accept(mut self) {
        self.accepted = true;
    }
}

#[cfg(any(target_os = "macos", test))]
impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.accepted {
            let watchers = board().owned.clone();
            for (_, watcher) in watchers {
                watcher(Update::Ended {
                    capture: self.capture,
                    submitted: false,
                });
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        board().owned.retain(|(id, _)| *id != self.0);
    }
}

struct Board {
    /// The publisher whose text is current, or 0 when none is.
    owner: u64,
    /// The current publisher's latest text.
    text: Option<Partial>,
    watchers: Vec<Watcher>,
    owned: Vec<(u64, OwnedWatcher)>,
}

static BOARD: Mutex<Board> = Mutex::new(Board {
    owner: 0,
    text: None,
    watchers: Vec::new(),
    owned: Vec::new(),
});
static NEXT_PUBLISHER: AtomicU64 = AtomicU64::new(1);
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
static NEXT_SUBSCRIPTION: AtomicU64 = AtomicU64::new(1);
static DEMANDS: AtomicUsize = AtomicUsize::new(0);

fn board() -> MutexGuard<'static, Board> {
    BOARD.lock().unwrap_or_else(|error| error.into_inner())
}

/// Calls `watcher` with each new text, and with `None` once the recording
/// that published it ends.
pub(crate) fn watch(watcher: impl Fn(Option<&Partial>) + Send + Sync + 'static) {
    watch_capture(move |_, partial| watcher(partial));
}

/// Display observers run before output observers so a word can be associated
/// with its recording before insertion is acknowledged on another thread.
pub(crate) fn watch_capture(watcher: impl Fn(CaptureId, Option<&Partial>) + Send + Sync + 'static) {
    board().watchers.push(Arc::new(watcher));
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn watch_owned(watcher: impl Fn(Update<'_>) + Send + Sync + 'static) -> Subscription {
    let id = NEXT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed);
    board().owned.push((id, Arc::new(watcher)));
    Subscription(id)
}

/// The text the recording in progress has shown, if any.
pub(crate) fn current() -> Option<Partial> {
    board().text.clone()
}

/// Asks dictation streams to publish their text while it is held, without
/// the experiment (the first run's practice field). The HUD plate still
/// shows only with the experiment.
pub(crate) struct Demand(());

pub(crate) fn demand() -> Demand {
    DEMANDS.fetch_add(1, Ordering::AcqRel);
    Demand(())
}

impl Drop for Demand {
    fn drop(&mut self) {
        DEMANDS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Whether anything holds a [`Demand`].
pub(crate) fn demanded() -> bool {
    DEMANDS.load(Ordering::Acquire) > 0
}

/// One recording's hold on the live text.
pub(crate) struct Publisher {
    id: u64,
    submitted: bool,
    live_insertion_allowed: bool,
}

impl Publisher {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::for_capture(false)
    }

    pub(crate) fn for_capture(live_insertion_allowed: bool) -> Self {
        Self {
            id: NEXT_PUBLISHER.fetch_add(1, Ordering::Relaxed),
            submitted: false,
            live_insertion_allowed,
        }
    }

    /// Identifies this recording across its partials and finished clip.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn id(&self) -> CaptureId {
        self.id
    }

    pub(crate) fn mark_submitted(&mut self) {
        self.submitted = true;
    }

    /// Shows `partial`, replacing any other recording's text.
    pub(crate) fn show(&self, partial: &Partial) {
        let (watchers, owned) = {
            let mut board = board();
            board.owner = self.id;
            board.text = Some(partial.clone());
            (board.watchers.clone(), board.owned.clone())
        };
        for watcher in watchers {
            watcher(self.id, Some(partial));
        }
        for (_, watcher) in owned {
            watcher(Update::Partial {
                capture: self.id,
                partial,
                eligible: self.live_insertion_allowed,
            });
        }
    }
}

impl Drop for Publisher {
    /// Clears the text if it is still this recording's.
    fn drop(&mut self) {
        let (watchers, owned) = {
            let mut board = board();
            let watchers = if board.owner == self.id {
                board.owner = 0;
                board.text = None;
                board.watchers.clone()
            } else {
                Vec::new()
            };
            (watchers, board.owned.clone())
        };
        for watcher in watchers {
            watcher(self.id, None);
        }
        for (_, watcher) in owned {
            watcher(Update::Ended {
                capture: self.id,
                submitted: self.submitted,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The board is process-wide; these tests take turns with it.
    static TURN: Mutex<()> = Mutex::new(());

    fn turn() -> MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn said(stable: &str) -> Partial {
        Partial {
            stable: stable.into(),
            tentative: String::new(),
        }
    }

    /// Records every call a watcher gets, as the stable text or `None`.
    fn watched() -> Arc<Mutex<Vec<Option<String>>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        watch(move |partial| {
            log.lock()
                .unwrap()
                .push(partial.map(|partial| partial.stable.clone()));
        });
        seen
    }

    #[test]
    fn a_recording_shows_its_text_and_clears_it_when_it_ends() {
        let _turn = turn();
        let seen = watched();
        let recording = Publisher::new();
        recording.show(&said("so the plan"));
        recording.show(&said("so the plan for today"));
        assert_eq!(current().unwrap().stable, "so the plan for today");
        drop(recording);
        assert_eq!(current(), None);
        assert_eq!(
            *seen.lock().unwrap(),
            [
                Some("so the plan".into()),
                Some("so the plan for today".into()),
                None
            ]
        );
    }

    #[test]
    fn an_older_recording_ending_never_clears_a_newer_ones_text() {
        let _turn = turn();
        let seen = watched();
        let older = Publisher::new();
        older.show(&said("old"));
        let newer = Publisher::new();
        newer.show(&said("new"));
        drop(older);
        assert_eq!(
            *seen.lock().unwrap(),
            [Some("old".into()), Some("new".into())]
        );
        drop(newer);
        assert_eq!(seen.lock().unwrap().last(), Some(&None));
    }

    #[test]
    fn a_recording_that_never_showed_text_clears_nothing() {
        let _turn = turn();
        let seen = watched();
        drop(Publisher::new());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn owned_endings_keep_capture_identity_when_a_newer_recording_is_visible() {
        let _turn = turn();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let subscription = watch_owned(move |update| {
            if let Update::Ended { capture, submitted } = update {
                log.lock().unwrap().push((capture, submitted));
            }
        });
        let mut older = Publisher::for_capture(true);
        older.show(&said("old"));
        older.mark_submitted();
        let old_id = older.id();
        let newer = Publisher::new();
        newer.show(&said("new"));
        drop(older);
        assert_eq!(current().unwrap().stable, "new");
        assert_eq!(*seen.lock().unwrap(), [(old_id, true)]);
        drop(subscription);
        drop(newer);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn only_an_unaccepted_ticket_retracts_its_submitted_draft() {
        let _turn = turn();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let _subscription = watch_owned(move |update| {
            if let Update::Ended { capture, submitted } = update {
                log.lock().unwrap().push((capture, submitted));
            }
        });
        let dropped = Ticket::new(NEXT_PUBLISHER.fetch_add(1, Ordering::Relaxed));
        let dropped_id = dropped.id();
        drop(dropped);
        Ticket::new(NEXT_PUBLISHER.fetch_add(1, Ordering::Relaxed)).accept();
        assert_eq!(*seen.lock().unwrap(), [(dropped_id, false)]);
    }

    #[test]
    fn display_identity_arrives_before_live_output_and_eligibility_is_explicit() {
        let _turn = turn();
        let displayed = Arc::new(AtomicU64::new(0));
        let display = displayed.clone();
        watch_capture(move |capture, partial| {
            if partial.is_some() {
                display.store(capture, Ordering::Release);
            }
        });
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let _subscription = watch_owned(move |update| {
            if let Update::Partial {
                capture, eligible, ..
            } = update
            {
                assert_eq!(displayed.load(Ordering::Acquire), capture);
                log.lock().unwrap().push(eligible);
            }
        });
        Publisher::new().show(&said("display only"));
        Publisher::for_capture(true).show(&said("eligible output"));
        assert_eq!(*seen.lock().unwrap(), [false, true]);
    }
}
