use std::collections::BTreeMap;

use super::*;
use crate::accessibility::LiveInsertion;
use crate::dictation::live_text::CaptureId;

#[derive(Default)]
pub(super) struct Drafts {
    entries: BTreeMap<CaptureId, Draft>,
    stopped_through: CaptureId,
}

enum Draft {
    Active {
        edit: LiveInsertion,
        revision: u64,
        format: Format,
    },
    Skipped,
    Paused(String),
}

struct Format {
    previous: Option<String>,
    preceding: Option<char>,
}

impl Format {
    fn apply(&self, text: &str) -> String {
        if let Some(previous) = &self.previous {
            return join(previous, text);
        }
        if crate::labs::smart_spacing()
            && self.preceding.is_some_and(|c| needs_leading_space(c, text))
        {
            return format!(" {text}");
        }
        text.to_owned()
    }
}

fn allowed(activity: &InputActivity, revision: u64, pid: i32, deadline: Instant) -> bool {
    Instant::now() < deadline
        && activity.physical_revision() == revision
        && crate::labs::words_to_caret()
        && !crate::accessibility::secure_input()
        && crate::dictation::context::frontmost_pid() == Some(pid)
}

/// Preserve every byte while exposing word-sized insertions, including their
/// preceding whitespace. A trailing separator belongs to its preceding word.
fn pieces(text: &str) -> Vec<&str> {
    let mut ranges = Vec::new();
    let mut end = 0;
    for word in text.split_whitespace() {
        let start = end;
        end += text[end..].find(word).expect("word came from this suffix") + word.len();
        ranges.push(start..end);
    }
    if let Some(last) = ranges.last_mut() {
        last.end = text.len();
    } else if !text.is_empty() {
        ranges.push(0..text.len());
    }
    ranges.into_iter().map(|r| &text[r]).collect()
}

impl Paster {
    pub(crate) fn live_partial(&mut self, capture: CaptureId, stable: &str, admit: bool) {
        if capture <= self.live.stopped_through {
            return;
        }
        if stable.trim().is_empty() {
            return;
        }
        let enabled = crate::labs::words_to_caret() && !crate::labs::copy_only();
        if !self.live.entries.contains_key(&capture) {
            if self.live.entries.len() >= 64 {
                self.cancel_all_live();
            }
            if capture <= self.live.stopped_through {
                return;
            }
            let draft = if enabled && admit && stable.len() <= 64 * 1024 {
                self.begin_live()
            } else {
                Draft::Skipped
            };
            self.live.entries.insert(capture, draft);
        }
        let Some(Draft::Active {
            edit,
            revision,
            format,
        }) = self.live.entries.get_mut(&capture)
        else {
            return;
        };
        if !enabled {
            let draft = if edit.touched() {
                Draft::Paused("live insertion was turned off".into())
            } else {
                Draft::Skipped
            };
            self.live.entries.insert(capture, draft);
            return;
        }
        let formatted = format.apply(stable);
        let Some(delta) = formatted.strip_prefix(edit.inserted()) else {
            return;
        };
        let parts: Vec<String> = pieces(delta)
            .into_iter()
            .take(32)
            .map(str::to_owned)
            .collect();
        let batch_deadline = Instant::now() + Duration::from_secs(1);
        for piece in parts {
            if Instant::now() >= batch_deadline {
                break;
            }
            let pid = edit.pid();
            let deadline = batch_deadline.min(Instant::now() + Duration::from_millis(400));
            let check = || allowed(&self.activity, *revision, pid, deadline);
            if let Err(error) = edit.append(&piece, &check) {
                let failed = if edit.touched() {
                    Draft::Paused(error)
                } else {
                    Draft::Skipped
                };
                self.live.entries.insert(capture, failed);
                return;
            }
            let words = edit.recent_words(&piece, &check);
            crate::words_to_caret::inserted(capture, words);
        }
    }

    fn begin_live(&self) -> Draft {
        let revision = self.activity.physical_revision();
        let continuation_revision = self.activity.revision();
        let Some(pid) = crate::dictation::context::frontmost_pid() else {
            return Draft::Skipped;
        };
        let previous = self
            .continuation
            .as_ref()
            .filter(|p| p.revision == continuation_revision)
            .map(|p| p.inserted.clone());
        let preceding = (previous.is_none() && crate::labs::smart_spacing())
            .then(|| crate::accessibility::char_before_caret(pid))
            .flatten();
        let deadline = Instant::now() + Duration::from_millis(300);
        match LiveInsertion::begin(pid, &|| allowed(&self.activity, revision, pid, deadline)) {
            Ok(edit) => Draft::Active {
                edit,
                revision,
                format: Format {
                    previous,
                    preceding,
                },
            },
            Err(_) => Draft::Skipped,
        }
    }

    pub(crate) fn live_end(&mut self, capture: CaptureId, submitted: bool) {
        if submitted {
            return;
        }
        let Some(draft) = self.live.entries.remove(&capture) else {
            return;
        };
        let Draft::Active {
            mut edit, revision, ..
        } = draft
        else {
            if matches!(draft, Draft::Paused(_)) {
                self.live.entries.insert(capture, draft);
            }
            return;
        };
        let pid = edit.pid();
        let deadline = Instant::now() + Duration::from_millis(400);
        if let Err(error) = edit.rollback(&|| allowed(&self.activity, revision, pid, deadline)) {
            tracing::warn!(%error, "live dictation left in place after ownership changed");
            if edit.touched() {
                self.live.entries.insert(capture, Draft::Paused(error));
            }
        }
    }

    pub(crate) fn cancel_all_live(&mut self) {
        if let Some(last) = self.live.entries.last_key_value().map(|(id, _)| *id) {
            self.live.stopped_through = self.live.stopped_through.max(last);
        }
        let captures: Vec<_> = self.live.entries.keys().copied().collect();
        for capture in captures {
            self.live_end(capture, false);
        }
        self.live.entries.clear();
    }

    pub(crate) fn finish_capture(
        &mut self,
        capture: Option<CaptureId>,
        text: &str,
        mode: PasteMode,
        commit: impl FnOnce() -> bool,
    ) -> Result<()> {
        if capture.is_some_and(|id| id <= self.live.stopped_through)
            && !matches!(mode, PasteMode::CopyOnly)
        {
            return Err(eyre!(
                "Live dictation stopped. The final text is available in History and Paste Last."
            ));
        }
        let draft = capture.and_then(|id| self.live.entries.remove(&id));
        match draft {
            Some(Draft::Active {
                mut edit,
                revision,
                format,
            }) => {
                let pid = edit.pid();
                let deadline = Instant::now() + Duration::from_millis(500);
                let check = || allowed(&self.activity, revision, pid, deadline);
                if matches!(mode, PasteMode::CopyOnly) {
                    let _ = edit.rollback(&check);
                    return self.copy_only(text, commit);
                }
                if !commit() {
                    return Err(eyre!("dictation was cancelled"));
                }
                let final_text = format.apply(text);
                edit.replace(&final_text, &check).map_err(|error| eyre!("Live dictation stopped: {error}. The final text is available in History and Paste Last."))?;
                self.continuation = Some(Continuation {
                    revision: self.activity.revision(),
                    inserted: final_text,
                });
                Ok(())
            }
            Some(Draft::Paused(reason)) if !matches!(mode, PasteMode::CopyOnly) => Err(eyre!(
                "Live dictation stopped: {reason}. The final text is available in History and Paste Last."
            )),
            _ => self.paste(text, mode, commit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_insertions_preserve_whitespace_unicode_and_punctuation() {
        for text in ["Hello world.", " 👋 café 世界\nnext ", "\n\t", "word", ""] {
            assert_eq!(pieces(text).concat(), text);
        }
        assert_eq!(pieces(" one  two\nthree "), [" one", "  two", "\nthree "]);
    }

    #[test]
    fn final_format_uses_the_context_before_live_typing() {
        let f = Format {
            previous: Some("First sentence".into()),
            preceding: None,
        };
        assert_eq!(f.apply("The next words"), " the next words");
        assert_eq!(f.apply("The final correction"), " the final correction");
    }
}
