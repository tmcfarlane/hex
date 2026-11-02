//! Scripted native rehearsal: no microphone, Accessibility, or observers.

use rune::{AppSpec, TextEdit, col};
use rune_kit::TextInput;

use super::*;

const SIZE: (f32, f32) = (760.0, 360.0);
const LINE: &str = "Words go where you type.";

fn fixture(s: &mut Flights) {
    *s = Flights {
        fixture: true,
        size: [SIZE.0, SIZE.1],
        ..Default::default()
    };
    s.fixture_field.focused = true;
}

pub(crate) fn preview(args: Vec<String>) {
    let spec = AppSpec {
        scenes: &[("hex-buddy", fixture)],
        fonts: rune_theme_opencode::FONTS,
        families: rune_theme_opencode::FAMILIES,
        poll: Some(poll),
        ..AppSpec::new(
            "Hex buddy",
            SIZE,
            || {
                let mut s = Flights::default();
                fixture(&mut s);
                s
            },
            view,
        )
    };
    rune::App::new(spec)
        .compositor(BuddyCompositor::default())
        .args(args)
        .run();
}

fn poll(s: &mut Flights, time: f64, tx: &mut Tx) -> bool {
    let was_active = s.active();
    let had_words = !s.words.is_empty();
    let previous_step = s.fixture_step;
    let now = time;
    s.now = now;
    if s.fixture_step == 0 {
        s.handle(BuddyEvent::Hud(HudEvent::Started), 1, [155.0, 35.0]);
        s.handle(
            BuddyEvent::Target {
                generation: 1,
                target: Some(ScreenRect {
                    x: 296.0,
                    y: 237.0,
                    width: 1.0,
                    height: 16.0,
                }),
            },
            1,
            [155.0, 35.0],
        );
        s.fixture_step = 1;
    }
    let words: Vec<_> = LINE.split_whitespace().collect();
    let landings = landings(&words);
    for count in 1..=words.len() {
        if now >= 0.2 + count as f64 * 0.45 && s.fixture_step <= count {
            let stable = words[..count].join(" ");
            s.handle(
                BuddyEvent::Partial {
                    generation: 1,
                    capture: 1,
                    tentative: words.get(count).unwrap_or(&"").to_string(),
                },
                1,
                [155.0, 35.0],
            );
            s.fixture_field = TextEdit::new(stable);
            s.fixture_field.focused = true;
            s.handle(
                BuddyEvent::Inserted {
                    capture: 1,
                    words: vec![landings[count - 1].clone()],
                },
                1,
                [155.0, 35.0],
            );
            s.fixture_step = count + 1;
        }
    }
    if now >= 3.1 && s.fixture_step == words.len() + 1 {
        s.handle(
            BuddyEvent::Hud(HudEvent::Submitted { job_id: 1 }),
            1,
            [155.0, 35.0],
        );
        s.fixture_step += 1;
    }
    if now >= 3.5 && s.fixture_step == words.len() + 2 {
        s.handle(
            BuddyEvent::Hud(HudEvent::JobCompleted { job_id: 1 }),
            1,
            [155.0, 35.0],
        );
        s.fixture_step += 1;
    }
    s.advance(now, true, s.reduced);
    s.level = if s.recording {
        0.10 + 0.34 * ((now * 9.0).sin() as f32).abs()
    } else {
        0.0
    };
    tx.instant();
    let active = s.active();
    if active {
        tx.wake_after(Duration::from_millis(16));
    }
    had_words || !s.words.is_empty() || active != was_active || previous_step != s.fixture_step
}

fn landings(words: &[&str]) -> Vec<(String, ScreenRect)> {
    let font = rune_theme_opencode::kit(true).ty.ui_compact;
    let space = rune::text::measure(" ", &[], font, None).0 as f64;
    let mut x = 296.0;
    words
        .iter()
        .map(|word| {
            let width = rune::text::measure(word, &[], font, None).0 as f64;
            let rect = ScreenRect {
                x,
                y: 237.0,
                width,
                height: 16.0,
            };
            x += width + space;
            (word.to_string(), rect)
        })
        .collect()
}

pub(super) fn field(s: &Flights, p: Paint) -> El {
    col()
        .key("destination")
        .absolute(288.0, 209.0)
        .w(430.0)
        .child(text("Draft").font(p.k.ty.small).color(p.muted()).mb(10.0))
        .child(
            TextInput::new(&s.fixture_field)
                .placeholder("Write something…")
                .on_edit(|s: &mut Flights| &mut s.fixture_field)
                .build(p.k)
                .key("draft-field")
                .w_full(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_field_grows_by_word_and_release_does_not_replay_it() {
        let mut s = Flights::default();
        fixture(&mut s);
        poll(&mut s, 0.0, &mut Tx::default());
        assert!(s.fixture_field.text.is_empty() && s.words.is_empty());
        let words: Vec<_> = LINE.split_whitespace().collect();
        for count in 1..=words.len() {
            poll(&mut s, 0.21 + count as f64 * 0.45, &mut Tx::default());
            assert_eq!(s.fixture_field.text, words[..count].join(" "));
            assert_eq!(s.words.last().unwrap().text, words[count - 1]);
        }
        poll(&mut s, 3.11, &mut Tx::default());
        assert_eq!(s.fixture_field.text, LINE);
        assert!(!s.recording && s.pending.is_empty());
        assert!(s.words.is_empty());
        poll(&mut s, 3.51, &mut Tx::default());
        assert_eq!(s.fixture_field.text, LINE);
        assert!(s.jobs.is_empty() && s.words.is_empty());
        poll(&mut s, 4.7, &mut Tx::default());
        assert_eq!(s.fixture_field.text, LINE);
        assert!(!s.active() && s.pending.is_empty() && s.words.is_empty());
    }

    #[test]
    fn reduced_motion_still_updates_the_real_field() {
        let mut s = Flights::default();
        fixture(&mut s);
        s.reduced = true;
        poll(&mut s, 0.66, &mut Tx::default());
        assert_eq!(s.fixture_field.text, "Words");
        assert_eq!(s.preview, "go");
        assert!(s.words.is_empty() && s.active());
    }
}
