//! The homepage character listens beside the HUD and sends visual words
//! toward the caret. Partial words never insert text; final flights follow
//! the normal paste. The Rune overlay passes every click through.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use rune::compose::{Compositor, DeviceKey, Frame};
use rune::{Anim, Cx, El, OverlayHandle, Tx, el, text};

use super::controls::Paint;
use super::hex_buddy_painter::{Draw, Painter};
use super::onboarding_kit::flight::{Pose, flying};
use crate::accessibility::ScreenRect;
use crate::rune_hud::HudEvent;
use crate::words_to_caret::{BuddyEvent, Launch, Tuning};

/// The word's weight in the air.
const WEIGHT: f32 = 560.0;
/// Room around the flight for the glow and the overshoot.
const MARGIN: f32 = 48.0;

mod preview;
pub(crate) use preview::preview;

/// One word in the air, in global screen points.
#[derive(Clone, Debug, PartialEq)]
struct Word {
    text: String,
    from: [f32; 2],
    to: [f32; 2],
    /// The font size it lands at, from its rect's height.
    land: f32,
    start: f64,
    /// Which side the arc bows to.
    side: f32,
    tuning: Tuning,
    speculative: bool,
}

#[derive(Default)]
pub(crate) struct Flights {
    words: Vec<Word>,
    pending: Vec<Launch>,
    size: [f32; 2],
    origin: [f32; 2],
    buddy: [f32; 4],
    now: f64,
    generation: u64,
    recording: bool,
    jobs: BTreeSet<u64>,
    target: Option<ScreenRect>,
    seen: usize,
    preview: String,
    until: f64,
    reduced: bool,
    level: f32,
    placement_pending: bool,
    fixture: bool,
    fixture_step: usize,
    fixture_field: rune::TextEdit,
    overlay: Option<OverlayHandle<Flights>>,
    shown: bool,
}

static MOUTH: Mutex<Option<[f32; 2]>> = Mutex::new(None);

pub(crate) fn mouth_origin() -> Option<[f32; 2]> {
    *MOUTH.lock().unwrap_or_else(|error| error.into_inner())
}

impl Word {
    fn duration(&self, tuning: &Tuning) -> f64 {
        tuning.duration_ms / 1000.0
    }

    fn arrives(&self, tuning: &Tuning) -> f64 {
        self.start + self.duration(tuning)
    }

    fn gone(&self, tuning: &Tuning) -> f64 {
        self.arrives(tuning) + tuning.fade_ms / 1000.0
    }

    /// Where the word is `u` of the way through its flight (past 1 at the
    /// overshoot): a quadratic arc eased out with a little back.
    fn point(&self, u: f32, tuning: &Tuning) -> [f32; 2] {
        let c = tuning.overshoot.max(0.0);
        let x = u.clamp(0.0, 1.0) - 1.0;
        let s = 1.0 + (c + 1.0) * x * x * x + c * x * x;
        let [sx, sy] = self.from;
        let [gx, gy] = self.to;
        let (dx, dy) = (gx - sx, gy - sy);
        let bend = self.side * tuning.bend;
        let (cx, cy) = (sx + dx * 0.5 - dy * bend, sy + dy * 0.5 + dx * bend);
        // The ease runs past 1 on the far side of the slot, along the arc's
        // last tangent.
        let t = s.min(1.0);
        let over = (s - 1.0).max(0.0);
        let (a, b, c2) = ((1.0 - t) * (1.0 - t), 2.0 * (1.0 - t) * t, t * t);
        let (tx, ty) = (gx - cx, gy - cy);
        [
            a * sx + b * cx + c2 * gx + tx * over,
            a * sy + b * cy + c2 * gy + ty * over,
        ]
    }

    /// The landing spring's push along the arrival direction, `t` seconds in.
    fn spring(&self, t: f32, tuning: &Tuning) -> [f32; 2] {
        let w = std::f32::consts::TAU * tuning.land_hz.max(0.1);
        let zeta = tuning.land_damping.clamp(0.05, 0.95);
        let wd = w * (1.0 - zeta * zeta).sqrt();
        let k = tuning.land_push * (-zeta * w * t).exp() * (wd * t).sin();
        let a = self.point(0.98, tuning);
        let (dx, dy) = (self.to[0] - a[0], self.to[1] - a[1]);
        let d = dx.hypot(dy).max(1e-3);
        [dx / d * k, dy / d * k]
    }

    fn pose(&self, now: f64, tuning: &Tuning) -> Option<Pose> {
        let age = now - self.start;
        if age < 0.0 || now >= self.gone(tuning) {
            return None;
        }
        let dur = self.duration(tuning).max(1e-3);
        let sprite = tuning.launch_size.max(self.land);
        if age >= dur {
            let t = (age - dur) as f32;
            let fade = (t / (tuning.fade_ms.max(1.0) / 1000.0) as f32).clamp(0.0, 1.0);
            let [nx, ny] = self.spring(t, tuning);
            return Some(Pose {
                x: self.to[0] + nx,
                y: self.to[1] + ny,
                vx: 0.0,
                vy: 0.0,
                size: self.land,
                sprite,
                angle: 0.0,
                heat: 0.9 * tuning.glow.clamp(0.0, 1.0) * (1.0 - fade),
                alpha: (1.0 - fade) * (1.0 - fade),
            });
        }
        let u = (age / dur) as f32;
        let [x, y] = self.point(u, tuning);
        let step = 1.0 / 120.0;
        let [x0, y0] = self.point((u - step).max(0.0), tuning);
        let per_second = step * dur as f32;
        let (vx, vy) = ((x - x0) / per_second, (y - y0) / per_second);
        let e = smooth(0.15, 0.85, u);
        let mut angle = vy.atan2(vx);
        if angle > std::f32::consts::FRAC_PI_2 {
            angle -= std::f32::consts::PI;
        }
        if angle < -std::f32::consts::FRAC_PI_2 {
            angle += std::f32::consts::PI;
        }
        let grow = (u * 4.0).min(1.0);
        Some(Pose {
            x,
            y,
            vx,
            vy,
            size: lerp(tuning.launch_size * lerp(0.4, 1.0, grow), self.land, e),
            sprite,
            angle: lerp(angle * 0.2, 0.0, e),
            heat: lerp(0.35, 0.9, e) * tuning.glow.clamp(0.0, 1.0),
            alpha: grow.min(1.0),
        })
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The font size of text whose line is `height` points tall.
fn font_size(height: f64) -> f32 {
    (height as f32 / 1.25).clamp(9.0, 48.0)
}

/// The overlay's rect for a flight, in global points, and its words in the
/// overlay's points, launched `stagger` apart from `now`.
fn lay_out(launch: &Launch, now: f64) -> ([f32; 4], Vec<Word>) {
    let center = |rect: &ScreenRect| {
        let (x, y) = rect.center();
        [x as f32, y as f32]
    };
    let from = launch.from;
    let mut min = from;
    let mut max = from;
    let mut reach = 0.0_f32;
    for (_, rect) in &launch.words {
        let [x, y] = center(rect);
        min = [min[0].min(rect.x as f32), min[1].min(rect.y as f32)];
        max = [
            max[0].max((rect.x + rect.width) as f32),
            max[1].max((rect.y + rect.height) as f32),
        ];
        reach = reach.max((x - from[0]).hypot(y - from[1]));
    }
    // A quadratic arc bows half as far as its control point.
    let pad = MARGIN + reach * launch.tuning.bend.abs() * 0.5;
    let origin = [min[0] - pad, min[1] - pad];
    let rect = [
        origin[0],
        origin[1],
        max[0] - min[0] + pad * 2.0,
        max[1] - min[1] + pad * 2.0,
    ];
    let local = |[x, y]: [f32; 2]| [x - origin[0], y - origin[1]];
    let words = launch
        .words
        .iter()
        .enumerate()
        .map(|(index, (text, rect))| Word {
            text: text.clone(),
            from: local(from),
            to: local(center(rect)),
            land: font_size(rect.height),
            start: now + index as f64 * launch.tuning.stagger_ms / 1000.0,
            side: if index % 2 == 0 { -1.0 } else { 1.0 },
            tuning: launch.tuning,
            speculative: false,
        })
        .collect();
    (rect, words)
}

fn tail(value: &str, limit: usize) -> String {
    let chars = value.chars().count();
    value.chars().skip(chars.saturating_sub(limit)).collect()
}

impl Flights {
    fn active(&self) -> bool {
        self.recording || !self.jobs.is_empty() || self.now < self.until || !self.words.is_empty()
    }

    fn clear(&mut self) {
        self.words.clear();
        self.pending.clear();
        self.recording = false;
        self.jobs.clear();
        self.target = None;
        self.seen = 0;
        self.preview.clear();
        self.until = 0.0;
        self.level = 0.0;
        self.placement_pending = false;
    }

    fn draw(&self, output: [u32; 2], scale: f32) -> Draw {
        Draw {
            rect: [
                (self.buddy[0] - self.origin[0]) * scale,
                (self.buddy[1] - self.origin[1]) * scale,
                self.buddy[2] * scale,
                self.buddy[3] * scale,
            ],
            output,
            time: self.now as f32,
            level: if self.reduced { 0.0 } else { self.level },
            reduced: self.reduced,
        }
    }

    fn mouth(&self) -> [f32; 2] {
        let mouth = self.draw([1, 1], 1.0).mouth();
        [mouth[0] + self.origin[0], mouth[1] + self.origin[1]]
    }

    fn handle(&mut self, event: BuddyEvent, generation: u64, anchor: [f32; 2]) {
        match event {
            BuddyEvent::Hud(HudEvent::Started) => {
                self.generation = generation;
                self.recording = true;
                self.target = None;
                self.seen = 0;
                self.preview.clear();
                self.words.retain(|word| !word.speculative);
                self.buddy = [anchor[0] - 55.0, anchor[1] + 35.0, 110.0, 110.0];
                self.placement_pending = true;
            }
            BuddyEvent::Hud(HudEvent::Submitted { job_id }) if self.recording => {
                self.recording = false;
                self.jobs.insert(job_id);
                self.until = self.now + 1.0;
                self.preview.clear();
            }
            BuddyEvent::Hud(
                HudEvent::JobCompleted { job_id }
                | HudEvent::JobCancelled { job_id }
                | HudEvent::JobFailed { job_id },
            ) => {
                if self.jobs.remove(&job_id) {
                    self.until = self.now + 1.0;
                }
            }
            BuddyEvent::Hud(HudEvent::Cancelled | HudEvent::Discarded | HudEvent::Failed) => {
                self.recording = false;
                self.target = None;
                self.preview.clear();
                self.words.retain(|word| !word.speculative);
                self.until = 0.0;
            }
            BuddyEvent::Hud(HudEvent::Reset) => self.clear(),
            BuddyEvent::Target { generation, target } if generation == self.generation => {
                if self.target != target {
                    self.words.retain(|word| !word.speculative);
                }
                self.target = target;
            }
            BuddyEvent::Partial {
                generation,
                stable,
                tentative,
            } if generation == self.generation && self.recording => {
                self.preview = if self.reduced || tentative.trim().is_empty() {
                    tail(format!("{stable} {tentative}").trim(), 60)
                } else {
                    tail(&tentative, 60)
                };
                // Stable snapshots may revise their prefix. A position is emitted
                // once; a shorter or corrected snapshot never replays old words.
                let words: Vec<_> = stable.split_whitespace().collect();
                let appended = words.iter().skip(self.seen).copied().collect::<Vec<_>>();
                self.seen = self.seen.max(words.len());
                if !self.reduced
                    && let Some(target) = self.target
                {
                    let from = self.mouth();
                    let (x, y) = target.center();
                    let tuning = Tuning {
                        fade_ms: 65.0,
                        glow: 0.0,
                        land_push: 0.0,
                        overshoot: 0.0,
                        ..Tuning::default()
                    };
                    for (index, word) in appended
                        .into_iter()
                        .take(tuning.max_words)
                        .filter(|word| word.chars().count() <= 64)
                        .enumerate()
                    {
                        self.words.push(Word {
                            text: word.to_string(),
                            from,
                            to: [x as f32, y as f32],
                            land: font_size(target.height),
                            start: self.now + index as f64 * tuning.stagger_ms / 1000.0,
                            side: if index % 2 == 0 { -1.0 } else { 1.0 },
                            tuning,
                            speculative: true,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn advance(&mut self, now: f64, enabled: bool, reduced: bool) {
        self.now = now;
        self.reduced = reduced;
        if !enabled {
            self.clear();
            return;
        }
        if reduced {
            self.words.clear();
        } else {
            self.words.retain(|word| now < word.gone(&word.tuning));
        }
        for launch in std::mem::take(&mut self.pending) {
            if self.buddy[2] == 0.0 {
                self.buddy = [launch.from[0] - 55.0, launch.from[1] - 68.3, 110.0, 110.0];
            }
            if reduced {
                if !self.recording {
                    self.preview = tail(
                        &launch
                            .words
                            .iter()
                            .map(|(word, _)| word.as_str())
                            .collect::<Vec<_>>()
                            .join(" "),
                        60,
                    );
                }
                self.until = now + 0.6;
                continue;
            }
            let (rect, mut words) = lay_out(&launch, now);
            for word in &mut words {
                word.from = [word.from[0] + rect[0], word.from[1] + rect[1]];
                word.to = [word.to[0] + rect[0], word.to[1] + rect[1]];
            }
            self.words.extend(words);
            self.until = now + 0.6;
        }
        if self.words.len() > 64 {
            self.words.drain(..self.words.len() - 64);
        }
    }

    fn bounds(&self) -> [f32; 4] {
        let [x, y, w, h] = self.buddy;
        let mut min = [x - 100.0, y - 12.0];
        let mut max = [x + w + 100.0, y + h + 48.0];
        for word in &self.words {
            let distance = (word.to[0] - word.from[0]).hypot(word.to[1] - word.from[1]);
            let pad = MARGIN + distance * word.tuning.bend.abs() * 0.5;
            for p in [word.from, word.to] {
                min = [min[0].min(p[0] - pad), min[1].min(p[1] - pad)];
                max = [max[0].max(p[0] + pad), max[1].max(p[1] + pad)];
            }
        }
        [
            min[0].floor(),
            min[1].floor(),
            (max[0] - min[0]).ceil(),
            (max[1] - min[1]).ceil(),
        ]
    }
}

fn beside_hud(center: [f32; 2], below: bool) -> [f32; 4] {
    [
        center[0] - 55.0,
        center[1] + if below { 35.0 } else { -145.0 },
        110.0,
        110.0,
    ]
}

fn poll(s: &mut Flights, now: f64, tx: &mut Tx) -> bool {
    let enabled = crate::labs::words_to_caret() && !crate::accessibility::secure_input();
    s.advance(now, enabled, s.reduced);
    s.level = if s.recording {
        crate::audio::input_level::recent().unwrap_or(0.0)
    } else {
        0.0
    };
    let active = s.active();
    if active && s.placement_pending {
        // The HUD places itself on the same queue after Started. Let that
        // turn finish before reading its display and top/bottom placement.
        s.placement_pending = false;
        tx.wake_after(Duration::from_millis(16));
        return false;
    }
    if active {
        let center =
            crate::rune_hud::plate_center().unwrap_or_else(crate::words_to_caret::hud_spot);
        let below = match crate::rune_hud::live_text_place() {
            super::live_text::Spot::Global { below, .. } => below,
            super::live_text::Spot::Edge(edge, _) => edge == rune::ScreenEdge::Top,
        };
        let buddy = beside_hud(center, below);
        if buddy != s.buddy {
            s.buddy = buddy;
            s.words.retain(|word| !word.speculative);
        }
    }
    *MOUTH.lock().unwrap_or_else(|error| error.into_inner()) = active.then(|| s.mouth());
    if let Some(overlay) = &s.overlay {
        if active {
            let rect = s.bounds();
            if s.origin != [rect[0], rect[1]] || s.size != [rect[2], rect[3]] {
                s.origin = [rect[0], rect[1]];
                s.size = [rect[2], rect[3]];
                overlay.set_placement(Some(rune::OverlayPlacement::Rect {
                    x: rect[0],
                    y: rect[1],
                    width: rect[2],
                    height: rect[3],
                }));
                overlay.resize((rect[2], rect[3]));
            }
            if !s.shown {
                overlay.show();
            }
        } else if s.shown {
            overlay.hide();
        }
    }
    let changed = active || s.shown;
    s.shown = active;
    tx.instant();
    tx.wake_after(if active && !s.reduced {
        Duration::ZERO
    } else {
        Duration::from_millis(100)
    });
    changed
}

/// Adds the overlay without starting capture or reading another app.
pub(crate) fn install<S: 'static>(app: &mut rune::App<S>) {
    use rune::{DisplayTarget, OverlayInput, OverlayLevel, OverlayOptions, OverlaySpec};
    let spec = OverlaySpec::new("Hex buddy", (320.0, 240.0), view)
        .options(OverlayOptions {
            display: DisplayTarget::Primary,
            level: OverlayLevel::AboveStatus,
            input: OverlayInput::None,
            prewarm: crate::labs::words_to_caret(),
            ..Default::default()
        })
        .poll(poll)
        .compositor(BuddyCompositor::default());
    let overlay = app.overlay(spec, Flights::default());
    let handle = overlay.clone();
    overlay.update(move |s, _| s.overlay = Some(handle));
    let launches = overlay.clone();
    crate::words_to_caret::on_launch(move |launch| {
        if !crate::labs::words_to_caret() {
            return;
        }
        launches.update(move |s, tx| {
            s.pending.push(launch);
            tx.wake_after(Duration::ZERO);
        });
    });
    crate::words_to_caret::on_event(move |event| {
        if !crate::labs::words_to_caret()
            || matches!(&event, BuddyEvent::Hud(HudEvent::Meter { .. }))
        {
            return;
        }
        let generation = crate::words_to_caret::generation();
        let anchor =
            crate::rune_hud::plate_center().unwrap_or_else(crate::words_to_caret::hud_spot);
        overlay.update(move |s, tx| {
            s.handle(event, generation, anchor);
            tx.wake_after(Duration::ZERO);
        });
    });
}

fn view(s: &Flights, cx: &mut Cx<Flights>) -> El {
    let mut root = el()
        .key("hex-buddy")
        .size(s.size[0].max(1.0), s.size[1].max(1.0))
        .hover_motion_within(Anim::INSTANT, Anim::INSTANT);
    let p = Paint::new(cx.system_dark(), cx.env().scale);
    if s.fixture {
        root = root.child(preview::field(s, p));
    }
    if !s.active() {
        return root;
    }
    if !s.preview.trim().is_empty() {
        root = root.child(
            text(s.preview.clone())
                .key("preview")
                .absolute(
                    s.buddy[0] - s.origin[0] - 95.0,
                    s.buddy[1] - s.origin[1] + 112.0,
                )
                .w(300.0)
                .truncate_head()
                .font(p.k.ty.ui)
                .color(p.text())
                .px(10.0)
                .py(5.0)
                .radius(p.k.radius.md)
                .fill(p.k.c.bg_base),
        );
    }
    if !cx.system_reduced_motion() {
        for (index, word) in s.words.iter().enumerate() {
            if let Some(mut pose) = word.pose(s.now, &word.tuning) {
                pose.x -= s.origin[0];
                pose.y -= s.origin[1];
                root = root.child(flying(p, &format!("w{index}"), &word.text, WEIGHT, pose));
            }
        }
    }
    root
}

#[derive(Default)]
struct BuddyCompositor {
    gpu: Option<(DeviceKey, Painter)>,
}

impl Compositor<Flights> for BuddyCompositor {
    fn render(&mut self, f: &mut Frame<'_, Flights>) {
        f.rt.state.reduced = f.rt.reduced_motion;
        f.ui.prepare(
            f.device,
            f.queue,
            &mut f.rt.text.atlas,
            &f.rt.instances,
            f.size,
        );
        f.ui.encode_layers(f.encoder);
        let clear = if f.rt.state.fixture {
            let color = Paint::new(f.rt.env.system_dark, f.rt.env.scale).k.c.bg_base;
            if f.srgb_blend() {
                color.premul_srgb(1.0)
            } else {
                color.premul(1.0)
            }
        } else {
            [0.0; 4]
        };
        drop(f.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Hex buddy clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: f.target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear[0] as f64,
                        g: clear[1] as f64,
                        b: clear[2] as f64,
                        a: clear[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        if f.rt.state.active() {
            if self
                .gpu
                .as_ref()
                .is_none_or(|(key, painter)| *key != f.device_key() || painter.format != f.format)
            {
                self.gpu = Some((f.device_key(), Painter::new(f.device, f.format)));
            }
            let painter = &self.gpu.as_ref().expect("created above").1;
            let draw = f.rt.state.draw([f.size.0, f.size.1], f.rt.env.scale);
            painter.write(draw, |buffer, data| f.write_buffer(buffer, 0, data));
            painter.draw_over(f.encoder, f.target);
        }
        f.ui.draw_target_range_with(
            f.encoder,
            f.target,
            f.format,
            wgpu::LoadOp::Load,
            0..f.rt.instances.len() as u32,
            &mut |_, _| {},
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch {
            from: [700.0, 20.0],
            words: vec![
                (
                    "so".into(),
                    ScreenRect {
                        x: 200.0,
                        y: 500.0,
                        width: 14.0,
                        height: 16.0,
                    },
                ),
                (
                    "plan".into(),
                    ScreenRect {
                        x: 220.0,
                        y: 500.0,
                        width: 30.0,
                        height: 16.0,
                    },
                ),
            ],
            tuning: Tuning::default(),
        }
    }

    #[test]
    fn the_overlay_covers_the_hud_and_every_landing() {
        let (rect, words) = lay_out(&launch(), 10.0);
        let [x, y, w, h] = rect;
        assert!(x < 200.0 && y < 20.0);
        assert!(x + w > 700.0 && y + h > 516.0);
        assert_eq!(words[0].from, [700.0 - x, 20.0 - y]);
        assert_eq!(words[1].to, [235.0 - x, 508.0 - y]);
    }

    #[test]
    fn words_launch_in_turn_and_land_on_their_slots() {
        let tuning = Tuning::default();
        let (_, words) = lay_out(&launch(), 10.0);
        assert!(words[1].start > words[0].start);
        let first = words[0].pose(10.0, &tuning).unwrap();
        assert_eq!([first.x, first.y], words[0].from);
        let landed = words[0].pose(words[0].arrives(&tuning), &tuning).unwrap();
        assert_eq!([landed.x, landed.y], words[0].to);
        assert_eq!(landed.size, words[0].land);
        assert!(words[0].pose(words[0].gone(&tuning), &tuning).is_none());
        assert!(words[1].pose(10.0, &tuning).is_none());
    }

    #[test]
    fn a_word_overshoots_its_slot_then_settles() {
        let tuning = Tuning::default();
        let (_, words) = lay_out(&launch(), 0.0);
        let word = &words[0];
        let past = (1..100).any(|n| {
            let p = word.point(n as f32 / 100.0, &tuning);
            // It travels down and left; past the slot is lower.
            p[1] > word.to[1] + 0.5
        });
        assert!(past);
        assert_eq!(word.point(1.0, &tuning), word.to);
    }

    fn listening() -> Flights {
        let mut s = Flights::default();
        s.handle(BuddyEvent::Hud(HudEvent::Started), 1, [700.0, 20.0]);
        s.handle(
            BuddyEvent::Target {
                generation: 1,
                target: Some(launch().words[0].1),
            },
            1,
            [700.0, 20.0],
        );
        s
    }

    fn partial(s: &mut Flights, stable: &str) {
        s.handle(
            BuddyEvent::Partial {
                generation: 1,
                stable: stable.into(),
                tentative: String::new(),
            },
            1,
            [700.0, 20.0],
        );
    }

    #[test]
    fn corrected_stable_prefixes_and_duplicate_updates_never_replay() {
        let mut s = listening();
        partial(&mut s, "one two");
        assert_eq!(s.words.len(), 2);
        partial(&mut s, "one two");
        partial(&mut s, "won too");
        partial(&mut s, "won");
        assert_eq!(s.words.len(), 2);
        partial(&mut s, "won too three");
        assert_eq!(s.words.len(), 3);
        assert_eq!(s.words[2].text, "three");
        assert!(s.words.iter().all(|word| word.tuning.glow == 0.0));
    }

    #[test]
    fn an_old_job_finishing_keeps_a_newer_capture_visible() {
        let mut s = listening();
        s.handle(
            BuddyEvent::Hud(HudEvent::Submitted { job_id: 10 }),
            1,
            [700.0, 20.0],
        );
        s.handle(BuddyEvent::Hud(HudEvent::Started), 2, [700.0, 20.0]);
        s.handle(
            BuddyEvent::Hud(HudEvent::JobCompleted { job_id: 10 }),
            2,
            [700.0, 20.0],
        );
        s.advance(20.0, true, false);
        assert!(s.recording && s.active());
        assert_eq!(s.generation, 2);
        assert!(s.jobs.is_empty());
    }

    #[test]
    fn disabling_clears_capture_jobs_text_and_queued_flights() {
        let mut s = listening();
        partial(&mut s, "one two");
        s.jobs.insert(10);
        s.pending.push(launch());
        s.advance(1.0, false, false);
        assert!(!s.active());
        assert!(s.words.is_empty() && s.pending.is_empty() && s.jobs.is_empty());
        assert!(s.preview.is_empty() && s.target.is_none());
    }

    #[test]
    fn reduced_motion_keeps_the_text_and_character_without_flights() {
        let mut s = listening();
        s.reduced = true;
        partial(&mut s, "one two");
        s.pending.push(launch());
        s.advance(1.0, true, true);
        assert!(s.active());
        assert!(s.words.is_empty());
        assert_eq!(s.preview, "one two");
        assert!(s.draw([1000, 800], 1.0).reduced);
    }

    #[test]
    fn a_target_change_removes_words_bound_for_the_old_field() {
        let mut s = listening();
        partial(&mut s, "one two");
        s.handle(
            BuddyEvent::Target {
                generation: 1,
                target: None,
            },
            1,
            [700.0, 20.0],
        );
        assert!(s.words.is_empty());
        partial(&mut s, "one two three");
        assert!(s.words.is_empty());
    }

    #[test]
    fn the_character_stays_toward_the_inside_of_either_hud_edge() {
        let top = beside_hud([700.0, 20.0], true);
        let bottom = beside_hud([700.0, 880.0], false);
        assert!(top[1] > 20.0);
        assert!(bottom[1] + bottom[3] < 880.0);
    }
}
