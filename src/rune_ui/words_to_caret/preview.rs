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
    let now = time + 1.6;
    s.now = now;
    if s.fixture_step == 0 {
        s.handle(BuddyEvent::Hud(HudEvent::Started), 1, [155.0, 35.0]);
        s.handle(
            BuddyEvent::Target {
                generation: 1,
                target: Some(ScreenRect {
                    x: 305.0,
                    y: 240.0,
                    width: 1.0,
                    height: 20.0,
                }),
            },
            1,
            [155.0, 35.0],
        );
        s.fixture_step = 1;
    }
    let words: Vec<_> = LINE.split_whitespace().collect();
    for count in 1..=words.len() {
        if now >= 0.9 + count as f64 * 0.45 && s.fixture_step <= count {
            s.handle(
                BuddyEvent::Partial {
                    generation: 1,
                    stable: words[..count].join(" "),
                    tentative: words.get(count).unwrap_or(&"").to_string(),
                },
                1,
                [155.0, 35.0],
            );
            s.fixture_step = count + 1;
        }
    }
    if now >= 3.8 && s.fixture_step == words.len() + 1 {
        s.handle(
            BuddyEvent::Hud(HudEvent::Submitted { job_id: 1 }),
            1,
            [155.0, 35.0],
        );
        s.fixture_field = TextEdit::new(LINE);
        s.fixture_field.focused = true;
        let font = rune_theme_opencode::kit(true).ty.ui;
        let mut x = 304.0;
        let landings = words
            .iter()
            .map(|word| {
                let width = rune::text::measure(word, &[], font, None).0;
                let rect = ScreenRect {
                    x,
                    y: 240.0,
                    width: width as f64,
                    height: 20.0,
                };
                x += width as f64 + rune::text::measure(" ", &[], font, None).0 as f64;
                (word.to_string(), rect)
            })
            .collect();
        s.pending.push(Launch {
            from: s.mouth(),
            words: landings,
            tuning: Tuning::default(),
        });
        s.fixture_step += 1;
    }
    if now >= 4.3 && s.fixture_step == words.len() + 2 {
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
    tx.wake_after(Duration::from_millis(16));
    true
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
