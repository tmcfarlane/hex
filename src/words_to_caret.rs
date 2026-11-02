//! Labs › Hex buddy (macOS): recording-owned live words and final pasted
//! words leave the homepage character for the focused text cursor.
//!
//! The paste never waits for it. Successful insertion hands its actual text
//! to [`pasted`], and one thread of its own reads the
//! app through Accessibility (each call bounded by 50 ms): it waits up to
//! `wait_ms` for the text before the cursor to read as the dictation, then
//! sends every word to its verified rect. An app that cannot confirm the
//! pasted text gets no final flight. `rune_ui::words_to_caret` draws it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::accessibility::{self, CaretTarget, ScreenRect};
use crate::rune_hud::HudEvent;

/// The flight's timings: `~/.config/hex/words-to-caret.json`, read as each
/// flight launches, so an edit applies to the next dictation. Missing keys
/// keep these defaults.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct Tuning {
    /// Each word's time in the air.
    pub(crate) duration_ms: f64,
    /// The gap between one word's launch and the next.
    pub(crate) stagger_ms: f64,
    /// How far the arc bows from the straight line, as a share of its length.
    pub(crate) bend: f32,
    /// The ease-out-back overshoot at the landing (0 for none).
    pub(crate) overshoot: f32,
    /// A word's size as it leaves the mouth, in points.
    pub(crate) launch_size: f32,
    /// The landing's spring: its push in points, frequency, and damping.
    pub(crate) land_push: f32,
    pub(crate) land_hz: f32,
    pub(crate) land_damping: f32,
    /// How long a landed word glows over the real text before it's gone.
    pub(crate) fade_ms: f64,
    /// How warm a word glows in flight and as it lands (0 for plain ink).
    pub(crate) glow: f32,
    /// At most this many words fly; the rest of the dictation just pastes.
    pub(crate) max_words: usize,
    /// How long to wait for the app to show the pasted text.
    pub(crate) wait_ms: u64,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            duration_ms: 300.0,
            stagger_ms: 28.0,
            bend: 0.16,
            overshoot: 0.9,
            launch_size: 15.0,
            land_push: 10.0,
            land_hz: 8.5,
            land_damping: 0.55,
            fade_ms: 260.0,
            glow: 0.6,
            max_words: 14,
            wait_ms: 250,
        }
    }
}

impl Tuning {
    pub(crate) fn load() -> Self {
        let Ok(path) = crate::app_paths::words_to_caret_tuning_file() else {
            return Self::default();
        };
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes).map(Self::bounded).unwrap_or_else(|error| {
                tracing::warn!(%error, path = %path.display(), "words to cursor tuning ignored");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn bounded(mut self) -> Self {
        self.duration_ms = self.duration_ms.clamp(100.0, 1200.0);
        self.stagger_ms = self.stagger_ms.clamp(0.0, 80.0);
        self.bend = self.bend.clamp(-0.4, 0.4);
        self.overshoot = self.overshoot.clamp(0.0, 1.5);
        self.launch_size = self.launch_size.clamp(9.0, 48.0);
        self.land_push = self.land_push.clamp(0.0, 20.0);
        self.land_hz = self.land_hz.clamp(1.0, 20.0);
        self.land_damping = self.land_damping.clamp(0.05, 0.95);
        self.fade_ms = self.fade_ms.clamp(60.0, 800.0);
        self.glow = self.glow.clamp(0.0, 1.0);
        self.max_words = self.max_words.min(32);
        self.wait_ms = self.wait_ms.clamp(50, 500);
        self
    }
}

/// One flight, in Rune's global space (points from the primary display's
/// top-left, y down).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Launch {
    /// The character's mouth.
    pub(crate) from: [f32; 2],
    /// Each word and the rect it lands on, in order.
    pub(crate) words: Vec<(String, ScreenRect)>,
    pub(crate) tuning: Tuning,
}

enum Job {
    /// A dictation started in this app: Chromium needs a moment to wake its
    /// accessibility tree.
    Target {
        generation: u64,
        pid: i32,
    },
    Pasted {
        pid: i32,
        text: String,
    },
}

/// Observation only: neither these events nor the live words write text.
pub(crate) enum BuddyEvent {
    Hud(HudEvent),
    Target {
        generation: u64,
        target: Option<ScreenRect>,
    },
    Partial {
        generation: u64,
        stable: String,
        tentative: String,
    },
}

static LAUNCH: OnceLock<Box<dyn Fn(Launch) + Send + Sync>> = OnceLock::new();
static OBSERVER: OnceLock<Box<dyn Fn(BuddyEvent) + Send + Sync>> = OnceLock::new();
static JOBS: OnceLock<SyncSender<Job>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);
static RECORDING: AtomicBool = AtomicBool::new(false);

pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub(crate) fn on_event(observe: impl Fn(BuddyEvent) + Send + Sync + 'static) {
    let _ = OBSERVER.set(Box::new(observe));
    crate::dictation::live_text::watch(|partial| {
        let Some(partial) =
            partial.filter(|_| crate::labs::words_to_caret() && !accessibility::secure_input())
        else {
            return;
        };
        if !RECORDING.load(Ordering::Acquire) {
            return;
        }
        if let Some(observe) = OBSERVER.get() {
            observe(BuddyEvent::Partial {
                generation: generation(),
                stable: partial.stable.clone(),
                tentative: partial.tentative.clone(),
            });
        }
        request_target();
    });
}

/// Called on the HUD's reporting path. All Accessibility work is queued.
pub(crate) fn observe(event: HudEvent) {
    if !crate::labs::words_to_caret() {
        RECORDING.store(false, Ordering::Release);
        return;
    }
    match event {
        HudEvent::Started => {
            GENERATION.fetch_add(1, Ordering::AcqRel);
            RECORDING.store(true, Ordering::Release);
        }
        HudEvent::Submitted { .. }
        | HudEvent::Cancelled
        | HudEvent::Discarded
        | HudEvent::Reset
        | HudEvent::Failed => RECORDING.store(false, Ordering::Release),
        _ => {}
    }
    if let Some(observe) = OBSERVER.get() {
        observe(BuddyEvent::Hud(event));
    }
    if matches!(event, HudEvent::Started) {
        request_target();
    }
}

fn request_target() {
    if let Some(pid) = crate::dictation::context::frontmost_pid() {
        send(Job::Target {
            generation: generation(),
            pid,
        });
    }
}

/// Where flights go: the overlay, set once as it installs.
pub(crate) fn on_launch(launch: impl Fn(Launch) + Send + Sync + 'static) {
    let _ = LAUNCH.set(Box::new(launch));
}

/// `text` was just pasted. Never waits; a busy thread drops the flight.
pub(crate) fn pasted(text: &str) {
    if crate::labs::words_to_caret() && LAUNCH.get().is_some() && !text.trim().is_empty() {
        if let Some(pid) = crate::dictation::context::frontmost_pid() {
            send(Job::Pasted {
                pid,
                text: text.to_string(),
            });
        }
    }
}

fn send(job: Job) {
    let jobs = JOBS.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel(4);
        if let Err(error) = std::thread::Builder::new()
            .name("words-to-caret".into())
            .spawn(move || run(receiver))
        {
            tracing::warn!(%error, "could not start words to cursor");
        }
        sender
    });
    let _ = jobs.try_send(job);
}

fn run(jobs: Receiver<Job>) {
    while let Ok(job) = jobs.recv() {
        if !crate::labs::words_to_caret() {
            continue;
        }
        match job {
            Job::Target {
                generation: id,
                pid,
            } => {
                if id != generation()
                    || !RECORDING.load(Ordering::Acquire)
                    || accessibility::secure_input()
                {
                    continue;
                }
                let deadline = Instant::now() + Duration::from_millis(300);
                let allowed = || {
                    admitted(pid, deadline)
                        && id == generation()
                        && RECORDING.load(Ordering::Acquire)
                };
                accessibility::wake_text_markers(pid, &allowed);
                let focus = accessibility::caret_focus(pid, &allowed);
                let target = focus
                    .as_ref()
                    .and_then(|focus| match focus.target(&allowed) {
                        Some(CaretTarget::Caret(rect)) => Some(rect),
                        _ => None,
                    });
                if allowed()
                    && let Some(observe) = OBSERVER.get()
                {
                    observe(BuddyEvent::Target {
                        generation: id,
                        target,
                    });
                }
            }
            Job::Pasted { pid, text } => {
                let started = Instant::now();
                let launch = find(pid, &text);
                tracing::debug!(
                    words = launch.as_ref().map_or(0, |launch| launch.words.len()),
                    find_ms = started.elapsed().as_millis(),
                    "words to cursor"
                );
                if crate::labs::words_to_caret()
                    && !accessibility::secure_input()
                    && crate::dictation::context::frontmost_pid() == Some(pid)
                    && let (Some(launch), Some(send)) = (launch, LAUNCH.get())
                {
                    send(launch);
                }
            }
        }
    }
}

/// Every AX read and every delivery checks the same deadline and admission.
fn admitted(pid: i32, deadline: Instant) -> bool {
    Instant::now() < deadline
        && crate::labs::words_to_caret()
        && !accessibility::secure_input()
        && crate::dictation::context::frontmost_pid() == Some(pid)
}

/// Retain one focused element and verify the inserted words. A cursor-only
/// fallback cannot establish which field received this paste, so it is skipped.
fn find(pid: i32, text: &str) -> Option<Launch> {
    let tuning = Tuning::load();
    if tuning.max_words == 0 || text.trim().is_empty() {
        return None;
    }
    let started = Instant::now();
    let deadline = started + Duration::from_millis(tuning.wait_ms);
    let allowed = || admitted(pid, deadline);
    let focus = accessibility::caret_focus(pid, &allowed)?;
    let from = crate::rune_ui::words_to_caret::mouth_origin();
    let words: Vec<&str> = text.split_whitespace().take(tuning.max_words).collect();
    let words = loop {
        if !allowed() {
            return None;
        }
        if let Some(rects) = focus.words(text, tuning.max_words, &allowed) {
            break land_on_words(&words, &rects);
        }
        if !allowed() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(8));
    };
    if words.is_empty() || !focus.is_current(pid, &allowed) || !allowed() {
        return None;
    }
    Some(Launch {
        from,
        words,
        tuning,
    })
}

/// Where the HUD's plate sits before its first show: under the primary
/// display's menu bar, in the middle.
pub(crate) fn hud_spot() -> [f32; 2] {
    #[repr(C)]
    struct CgRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayBounds(display: u32) -> CgRect;
    }
    // SAFETY: plain CoreGraphics queries, safe from any thread.
    let bounds = unsafe { CGDisplayBounds(CGMainDisplayID()) };
    [
        (bounds.x + bounds.width / 2.0) as f32,
        (bounds.y + 44.0) as f32,
    ]
}

/// Each word onto its own rect; a word without a believable one stays put.
fn land_on_words(words: &[&str], rects: &[Option<ScreenRect>]) -> Vec<(String, ScreenRect)> {
    words
        .iter()
        .zip(rects)
        .filter_map(|(word, rect)| {
            rect.filter(|_| word.chars().count() <= 64)
                .map(|rect| (word.to_string(), rect))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64) -> ScreenRect {
        ScreenRect {
            x,
            y: 400.0,
            width: 30.0,
            height: 16.0,
        }
    }

    #[test]
    fn only_bounded_words_with_believable_rects_fly() {
        let oversized = "x".repeat(65);
        let words = ["so", &oversized, "the", "plan"];
        let landed = land_on_words(
            &words,
            &[Some(rect(10.0)), Some(rect(30.0)), None, Some(rect(60.0))],
        );
        assert_eq!(
            landed,
            vec![("so".into(), rect(10.0)), ("plan".into(), rect(60.0))]
        );
    }

    #[test]
    fn missing_tuning_keys_keep_their_defaults() {
        let tuning: Tuning = serde_json::from_str(r#"{"duration_ms": 500}"#).unwrap();
        assert_eq!(tuning.duration_ms, 500.0);
        assert_eq!(tuning.stagger_ms, Tuning::default().stagger_ms);
    }

    #[test]
    fn tuning_cannot_keep_the_worker_or_overlay_busy_without_bound() {
        let tuning: Tuning = serde_json::from_str(
            r#"{"wait_ms": 999999, "max_words": 999999, "duration_ms": 1e100, "stagger_ms": 1e100, "fade_ms": 1e100}"#,
        ).unwrap();
        let tuning = tuning.bounded();
        assert!(tuning.wait_ms <= 500);
        assert!(tuning.max_words <= 32);
        let last_word_gone_ms =
            tuning.duration_ms + tuning.stagger_ms * tuning.max_words as f64 + tuning.fade_ms;
        assert!(last_word_gone_ms < 5000.0);
    }
}
