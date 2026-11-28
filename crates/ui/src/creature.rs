//! 3D-projected animated matte-vinyl rounded-box creatures for Keel Bots & Dr. Keelee.
//!
//! Matches Keel's subagent 3D creature language (`subagent_entity.rs`): real perspective
//! projection (`Cam3D`), 7-slice extruded matte-vinyl rounded cubes, directional face
//! shading, specular top bevels, depth-sorted ears, and Disney's 12 Principles of
//! Animation (`dylantarre/animation-principles`):
//!
//! - **Squash & Stretch**: volume-preserving deformation (`sx * sy ≈ 1.0`).
//! - **Anticipation**: wind-up crouch before Dr. Keelee's eureka pops & customizer spins.
//! - **Pose-to-Pose + Arcs**: curved 3D trajectories with cubic / `ease_out_back` easing.
//! - **Follow-Through & Overlapping Action**: wild 3D Einstein hair tufts, sweeping
//!   mustache, doctor stethoscope, hero cape, and headgear lagging cleanly behind body motion.
//! - **Slow In & Slow Out / Timing & Weight**: `BotSize` subtly tunes bounce weight & tempo.
//! - **Secondary Action & Appeal**: Dr. Keelee's eureka stars & lab sparkles, natural
//!   irregular double-blinks, eye catchlights, and dynamic shrinking floor shadow.

use std::time::Instant;

use gpui::{
    AnyElement, Bounds, Hsla, PathBuilder, Pixels, Point, Window, div, fill, point, prelude::*, px,
    size,
};
use keel_proto::bots::{
    BotAppearance, BotAttire, BotColor, BotEyes, BotHair, BotMustache, BotSize,
};
use keel_theme::Tokens;

use crate::style::{c, zpx};

/// Live animation mood for a 3D bot creature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotMood {
    /// Alive resting cadence: slow breathing squash & stretch, periodic 3D curiosity glance, blinks.
    Idle,
    /// Active working choreography (Dr. Keelee: ponder → crouch → eureka hop + star; bots: buoyant work).
    Working,
    /// Attentive head tilt + wide eyes + pulsing pip when waiting on the user.
    Awaiting,
    /// Static/resting pose for compact non-distracting places (e.g. historical message bubbles).
    Still,
}

/// Render a live 3D matte-vinyl bot creature framelessly (no circular background or border).
#[allow(clippy::too_many_arguments)]
pub fn render_creature_avatar(
    bot_id: &str,
    appearance: BotAppearance,
    is_creator: bool,
    mood: BotMood,
    logical_size: f32,
    clock_epoch: Instant,
    t: Tokens,
    reduced: bool,
) -> AnyElement {
    let phase = (fnv1a(bot_id) % 1000) as f32 / 1000.0;
    let animate = !reduced && mood != BotMood::Still;
    let dim = zpx(logical_size);

    div()
        .size(dim)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::canvas(
                move |_, _, _| (),
                move |bounds, _, window, _| {
                    if animate {
                        window.request_animation_frame();
                    }
                    let elapsed = if animate {
                        Instant::now()
                            .saturating_duration_since(clock_epoch)
                            .as_secs_f32()
                    } else {
                        0.0
                    };
                    paint_bot_3d(
                        bounds,
                        appearance,
                        is_creator,
                        mood,
                        elapsed,
                        phase,
                        None,
                        animate,
                        t,
                        window,
                    );
                },
            )
            .size_full(),
        )
        .into_any_element()
}

/// Render a larger frameless 3D creature portrait for the DM welcome card or Customizer studio stage.
/// When `pop_at` is recent, plays a joyful Disney anticipation → hop + 3D show-off yaw spin → settle!
#[allow(clippy::too_many_arguments)]
pub fn render_creature_stage(
    bot_id: &str,
    appearance: BotAppearance,
    is_creator: bool,
    mood: BotMood,
    logical_size: f32,
    clock_epoch: Instant,
    pop_at: Option<Instant>,
    t: Tokens,
    reduced: bool,
) -> AnyElement {
    let phase = (fnv1a(bot_id) % 1000) as f32 / 1000.0;
    let animate = !reduced && mood != BotMood::Still;
    let dim = zpx(logical_size);

    div()
        .size(dim)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::canvas(
                move |_, _, _| (),
                move |bounds, _, window, _| {
                    let pop_age = pop_at.map(|at| Instant::now().saturating_duration_since(at).as_secs_f32());
                    let popping = !reduced && pop_age.is_some_and(|a| a < 0.68);
                    if animate || popping {
                        window.request_animation_frame();
                    }
                    let elapsed = if animate {
                        Instant::now()
                            .saturating_duration_since(clock_epoch)
                            .as_secs_f32()
                    } else {
                        0.0
                    };
                    paint_bot_3d(
                        bounds,
                        appearance,
                        is_creator,
                        mood,
                        elapsed,
                        phase,
                        if popping { pop_age } else { None },
                        animate || popping,
                        t,
                        window,
                    );
                },
            )
            .size_full(),
        )
        .into_any_element()
}

/// Natural height (px) of the Bot typing loader bubble (`pt_2` + `34px` bubble + `pb_1`).
pub const BOT_LOADER_H: f32 = 46.0;

/// Render the messaging-app Bot typing/working loader:
/// a crisp chat bubble (`• • •`) with staggered bouncing dots matching the bot reply bubble's
/// exact curvature and alignment. Honors `look` (`RowLook`) so entering and handing off to a
/// bot reply smoothly glide in height, vertical offset, and opacity instead of snapping.
#[allow(clippy::too_many_arguments)]
pub fn render_bot_typing_loader(
    _bot_id: &str,
    appearance: BotAppearance,
    _is_creator: bool,
    clock_epoch: Instant,
    t: Tokens,
    reduced: bool,
    look: Option<crate::motion::RowLook>,
) -> AnyElement {
    let accent = c(appearance.color.accent_rgba());
    let animate = !reduced;
    let look = look.unwrap_or(crate::motion::RowLook {
        opacity: 1.,
        dy: 0.,
        height: 1.,
    });
    let dy = if reduced { 0. } else { look.dy };

    let bubble = div()
        .h(zpx(34.))
        .px_3p5()
        .rounded(zpx(16.))
        .rounded_bl(zpx(4.))
        .bg(c(t.surface))
        .border_1()
        .border_color(accent.opacity(0.22))
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::canvas(
                move |_, _, _| (),
                move |bounds, _, window, _| {
                    if animate {
                        window.request_animation_frame();
                    }
                    let elapsed = if animate {
                        Instant::now()
                            .saturating_duration_since(clock_epoch)
                            .as_secs_f32()
                    } else {
                        0.0
                    };
                    let cx = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5;
                    let cy = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;
                    let zoom = crate::style::zoom();
                    let r = 2.6 * zoom;
                    let spacing = 8.5 * zoom;
                    for i in 0..3 {
                        let phase = elapsed * 5.2 - (i as f32) * 0.72;
                        let wave = if animate {
                            phase.sin().max(0.0).powf(1.5)
                        } else {
                            0.45
                        };
                        let dot_dy = -wave * 3.4 * zoom;
                        let alpha = 0.40 + 0.60 * wave;
                        let x = cx + (i as f32 - 1.0) * spacing;
                        let y = cy + dot_dy;
                        window.paint_quad(
                            fill(
                                Bounds::new(
                                    point(px(x - r), px(y - r)),
                                    size(px(r * 2.0), px(r * 2.0)),
                                ),
                                accent.opacity(alpha),
                            )
                            .corner_radii(px(r)),
                        );
                    }
                },
            )
            .w(zpx(28.))
            .h(zpx(14.)),
        );

    div()
        .w_full()
        .flex_none()
        .overflow_hidden()
        .h(zpx(BOT_LOADER_H * look.height))
        .px_4()
        .flex()
        .flex_col()
        .items_start()
        .child(
            div()
                .relative()
                .top(zpx(dy))
                .opacity(look.opacity)
                .pt_2()
                .pb_1()
                .child(bubble),
        )
        .into_any_element()
}

/// Deterministic fun randomizer for the Customizer's "Surprise" button.
pub fn random_appearance(seed: u64) -> BotAppearance {
    let pick = |len: usize, shift: u32| -> usize {
        let mut x = seed.wrapping_add((shift as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((x ^ (x >> 31)) as usize) % len
    };
    BotAppearance {
        color: BotColor::ALL[pick(BotColor::ALL.len(), 1)],
        attire: BotAttire::ALL[pick(BotAttire::ALL.len(), 2)],
        eyes: BotEyes::ALL[pick(BotEyes::ALL.len(), 3)],
        hair: BotHair::ALL[pick(BotHair::ALL.len(), 4)],
        mustache: BotMustache::ALL[pick(BotMustache::ALL.len(), 5)],
        size: BotSize::ALL[pick(BotSize::ALL.len(), 6)],
    }
}

// ---------------------------------------------------------------------------
// 3D Vector & Camera Projection (matching `subagent_entity.rs`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

impl Vec3 {
    const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
}

#[derive(Clone, Copy)]
struct Cam3D {
    ox: f32,
    oy: f32,
    scale: f32,
    cx: f32,
    cy: f32,
    sx: f32,
    sy: f32,
    yaw: f32,
    pitch: f32,
    roll: f32,
}

impl Cam3D {
    const CAM_Z: f32 = 44.0;

    fn xform(&self, v: Vec3) -> Vec3 {
        let mut x = v.x * self.sx;
        let mut y = v.y * self.sy;
        let mut z = v.z;

        // Roll (around Z)
        let (sr, cr) = self.roll.sin_cos();
        let (x1, y1) = (x * cr - y * sr, x * sr + y * cr);
        x = x1;
        y = y1;

        // Pitch (around X): positive pitches top toward camera (nodding down).
        let (sp, cp) = self.pitch.sin_cos();
        let (y2, z2) = (y * cp - z * sp, y * sp + z * cp);
        y = y2;
        z = z2;

        // Yaw (around Y): positive turns face toward +X (viewer's right).
        let (syaw, cyaw) = self.yaw.sin_cos();
        let (x3, z3) = (x * cyaw + z * syaw, -x * syaw + z * cyaw);
        Vec3::new(x3, y, z3)
    }

    fn project(&self, v: Vec3) -> (Point<Pixels>, f32, f32) {
        let r = self.xform(v);
        let persp = Self::CAM_Z / (Self::CAM_Z - r.z).max(8.0);
        let px_x = self.ox + (self.cx + r.x * persp) * self.scale;
        let px_y = self.oy + (self.cy + r.y * persp) * self.scale;
        (point(px(px_x), px(px_y)), persp, r.z)
    }

    fn pt(&self, v: Vec3) -> Point<Pixels> {
        self.project(v).0
    }
}

// ---------------------------------------------------------------------------
// Disney 12 Principles Kinematics (`dylantarre/animation-principles`)
// ---------------------------------------------------------------------------

fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) * 0.5
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn arc(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    4.0 * t * (1.0 - t)
}

/// Volume-preserving squash & stretch (`sx * sy ≈ 1.0`).
fn squash_stretch(stretch: f32) -> (f32, f32) {
    let sy = (1.0 + stretch).clamp(0.80, 1.22);
    let sx = 1.0 / sy;
    (sx, sy)
}

#[derive(Clone, Copy, Debug)]
struct BotPose3D {
    bob_y: f32,
    lean_x: f32,
    sx: f32,
    sy: f32,
    yaw: f32,
    pitch: f32,
    roll: f32,
    gaze_x: f32,
    gaze_y: f32,
    eye_scale: f32,
    /// Overlapping follow-through lag for hair/beanie/cape (`dx`, `dy`).
    hair_dx: f32,
    hair_dy: f32,
    /// Secondary mustache wiggle / twitch (`-1..=1`).
    stache_wiggle: f32,
    /// Secondary sparkle / eureka star intensity (`0..=1`).
    fx: f32,
    /// Joyful smile intensity (`0..=1`).
    cheer: f32,
}

impl Default for BotPose3D {
    fn default() -> Self {
        Self {
            bob_y: 0.0,
            lean_x: 0.0,
            sx: 1.0,
            sy: 1.0,
            // Iconic 3/4 resting turn so the 3D matte-vinyl cube depth always reads crisply.
            yaw: -0.15,
            pitch: 0.08,
            roll: 0.0,
            gaze_x: 0.0,
            gaze_y: 0.0,
            eye_scale: 1.0,
            hair_dx: 0.0,
            hair_dy: 0.0,
            stache_wiggle: 0.0,
            fx: 0.0,
            cheer: 0.35,
        }
    }
}

/// Evaluate Dr. Keelee / Bot pose using Disney's 12 Principles:
/// weight-scaled tempo by `BotSize`, anticipation, arcs, overlapping lag, and appeal.
fn evaluate_bot_pose(
    is_creator: bool,
    size: BotSize,
    mood: BotMood,
    time: f32,
    pop_age: Option<f32>,
    animate: bool,
) -> BotPose3D {
    if !animate {
        return BotPose3D::default();
    }

    // Timing & Weight: small creatures feel slightly nimbler, large creatures feel weightier.
    let tempo = match size {
        BotSize::Small => 1.12,
        BotSize::Medium => 1.0,
        BotSize::Large => 0.90,
    };
    let t = time * tempo;

    let mut pose = match mood {
        BotMood::Still => BotPose3D::default(),
        BotMood::Awaiting => eval_awaiting_pose(t),
        BotMood::Working if is_creator => eval_dr_keelee_working_pose(t),
        BotMood::Working => eval_bot_working_pose(t),
        BotMood::Idle if is_creator => eval_dr_keelee_idle_pose(t),
        BotMood::Idle => eval_bot_idle_pose(t),
    };

    // Customizer "Show-Off" Pop: anticipation dip -> buoyant hop + playful 3D yaw wiggle -> elastic settle!
    if let Some(age) = pop_age {
        let dur = 0.66;
        let u = (age / dur).clamp(0.0, 1.0);
        if u < 0.16 {
            // Anticipation squash before spring pop!
            let k = ease_in_out_cubic(u / 0.16);
            let (sx, sy) = squash_stretch(-0.14 * k);
            pose.bob_y += 0.95 * k;
            pose.sx *= sx;
            pose.sy *= sy;
            pose.pitch += 0.12 * k;
            pose.hair_dy += 0.35 * k;
        } else if u < 0.56 {
            // Exaggerated joyful hop + 3D show-off turn left -> right!
            let k = (u - 0.16) / 0.40;
            let hop = arc(k);
            let (sx, sy) = squash_stretch(0.14 * hop);
            let spin = (k * std::f32::consts::TAU).sin();
            pose.bob_y += lerp(0.95, 0.0, ease_out_cubic(k)) - hop * 2.15;
            pose.sx *= sx;
            pose.sy *= sy;
            pose.yaw += spin * 0.42;
            pose.pitch += -0.16 * hop;
            pose.roll += -spin * 0.11;
            pose.eye_scale *= 1.0 + 0.14 * hop;
            // Overlapping follow-through: hair & mustache lag behind the spin & hop!
            pose.hair_dx += -spin * 0.95;
            pose.hair_dy += -hop * 0.65;
            pose.stache_wiggle += spin * 0.7;
            pose.fx = pose.fx.max(hop * 0.95);
            pose.cheer = 1.0;
        } else {
            // Elastic landing squash -> smooth settle!
            let k = (u - 0.56) / 0.44;
            let settle = ease_out_back(k);
            let damp = (1.0 - k).powi(2);
            let (sx, sy) = squash_stretch(-0.10 * damp * (k * std::f32::consts::PI).sin());
            pose.bob_y += 0.45 * damp * (1.0 - settle.min(1.0));
            pose.sx *= sx;
            pose.sy *= sy;
            pose.hair_dy += 0.55 * damp;
            pose.stache_wiggle += (k * std::f32::consts::TAU).sin() * 0.35 * damp;
            pose.fx = pose.fx.max((1.0 - k) * 0.5);
        }
    }

    pose
}

/// Dr. Keelee Idle (4.6 s): alive mad-scientist charm — breathing squash/stretch,
/// curious 3D head glance with mustache twitch, subtle eureka head-nod, and overlapping Einstein hair lag.
fn eval_dr_keelee_idle_pose(time: f32) -> BotPose3D {
    let u = time.rem_euclid(4.6) / 4.6;
    let breath = (time * 1.9).sin();
    let breath_lag = ((time - 0.16) * 1.9).sin();

    // Base breathing squash/stretch
    let mut stretch = breath * 0.032;
    let mut bob_y = -breath * 0.28;
    let mut lean_x = 0.0;
    let mut yaw = -0.15 + breath * 0.03;
    let mut pitch = 0.08 - breath * 0.02;
    let mut roll = 0.0;
    let mut gaze_x = 0.0;
    let mut gaze_y = 0.0;
    let mut eye_scale = 1.0;
    let mut hair_dx = -breath_lag * 0.16;
    let mut hair_dy = -breath_lag * 0.22;
    let mut stache_wiggle = 0.0;
    let mut fx = 0.0;
    let mut cheer = 0.45;

    if u > 0.20 && u < 0.54 {
        // Curious glance & thoughtful inspect to the right with mustache twitch!
        let k = (u - 0.20) / 0.34;
        let env = if k < 0.28 {
            ease_in_out_cubic(k / 0.28)
        } else if k < 0.72 {
            1.0
        } else {
            1.0 - ease_in_out_cubic((k - 0.72) / 0.28)
        };
        let lag_env = if k < 0.36 {
            ease_in_out_cubic((k / 0.36).clamp(0.0, 1.0))
        } else if k < 0.76 {
            1.0
        } else {
            1.0 - ease_in_out_cubic(((k - 0.76) / 0.24).clamp(0.0, 1.0))
        };
        yaw = lerp(-0.15, 0.24, env);
        pitch = lerp(0.08, -0.05, env);
        roll = 0.06 * env;
        lean_x = 0.35 * env;
        gaze_x = 0.65 * env;
        gaze_y = -0.20 * env;
        // Overlapping hair follow-through lags behind the head turn!
        hair_dx = (lag_env - env) * 1.15 - 0.18 * env;
        // Mad-scientist mustache twitch during the middle hold!
        if k > 0.32 && k < 0.68 {
            let tw = ((k - 0.32) / 0.36) * std::f32::consts::TAU * 2.0;
            stache_wiggle = tw.sin() * 0.65;
        }
    } else if u > 0.68 && u < 0.86 {
        // Mini spark-of-inspiration micro-hop & happy nod!
        let k = (u - 0.68) / 0.18;
        let hop = arc(k);
        if k < 0.24 {
            stretch -= 0.07 * (k / 0.24);
            bob_y += 0.4 * (k / 0.24);
        } else {
            stretch += 0.075 * hop;
            bob_y -= hop * 0.95;
            pitch -= 0.12 * hop;
            eye_scale = 1.0 + 0.09 * hop;
            hair_dy -= hop * 0.55;
            stache_wiggle = hop * 0.55;
            fx = hop * 0.72;
            cheer = 0.85;
        }
    }

    let (sx, sy) = squash_stretch(stretch);
    BotPose3D {
        bob_y,
        lean_x,
        sx,
        sy,
        yaw,
        pitch,
        roll,
        gaze_x,
        gaze_y,
        eye_scale,
        hair_dx,
        hair_dy,
        stache_wiggle,
        fx,
        cheer,
    }
}

/// Dr. Keelee Working (2.7 s): Pose-to-Pose "Inspect → Anticipation Crouch → Eureka Hop + Star → Tinker Nod"!
fn eval_dr_keelee_working_pose(time: f32) -> BotPose3D {
    let u = time.rem_euclid(2.7) / 2.7;
    if u < 0.32 {
        // Phase 1: Intense mad-scientist brainstorming (yaw left-right, eyes darting, hair swaying).
        let k = u / 0.32;
        let scan = (k * std::f32::consts::TAU).sin();
        let (sx, sy) = squash_stretch(-0.04 + scan.abs() * 0.03);
        BotPose3D {
            bob_y: 0.25 + scan.abs() * 0.25,
            lean_x: scan * 0.55,
            sx,
            sy,
            yaw: -0.10 + scan * 0.30,
            pitch: 0.14 - scan.abs() * 0.06,
            roll: -scan * 0.08,
            gaze_x: scan * 0.85,
            gaze_y: -0.25,
            eye_scale: 0.96,
            hair_dx: -scan * 0.75,
            hair_dy: 0.15,
            stache_wiggle: (k * std::f32::consts::TAU * 3.0).sin() * 0.5,
            fx: 0.0,
            cheer: 0.4,
        }
    } else if u < 0.44 {
        // Phase 2: Anticipation wind-up crouch before the eureka pop!
        let k = ease_in_out_cubic((u - 0.32) / 0.12);
        let (sx, sy) = squash_stretch(-0.14 * k);
        BotPose3D {
            bob_y: lerp(0.25, 1.15, k),
            lean_x: -0.35 * k,
            sx,
            sy,
            yaw: lerp(-0.10, -0.26, k),
            pitch: lerp(0.14, 0.24, k),
            roll: -0.07 * k,
            gaze_x: -0.2 * k,
            gaze_y: 0.25 * k,
            eye_scale: 0.90,
            hair_dx: 0.25 * k,
            hair_dy: 0.45 * k,
            stache_wiggle: -0.35 * k,
            fx: 0.0,
            cheer: 0.35,
        }
    } else if u < 0.66 {
        // Phase 3: Eureka Arc Hop + golden star burst + Einstein hair trailing upward!
        let k = (u - 0.44) / 0.22;
        let hop = arc(k);
        let (sx, sy) = squash_stretch(0.15 * hop);
        BotPose3D {
            bob_y: lerp(1.15, 0.0, ease_out_cubic(k)) - hop * 2.25,
            lean_x: lerp(-0.35, 0.25, ease_in_out_cubic(k)),
            sx,
            sy,
            yaw: lerp(-0.26, 0.20, ease_in_out_cubic(k)),
            pitch: lerp(0.24, -0.22, hop),
            roll: lerp(-0.07, 0.08, k),
            gaze_x: 0.15,
            gaze_y: -0.55 * hop,
            eye_scale: 1.0 + 0.18 * hop,
            hair_dx: -0.55 * hop,
            hair_dy: -0.95 * hop,
            stache_wiggle: hop * 0.9,
            fx: hop,
            cheer: 1.0,
        }
    } else {
        // Phase 4: Elastic landing squash & satisfied double-nod settle!
        let k = (u - 0.66) / 0.34;
        let rec = ease_out_back(k);
        let nod = (k * std::f32::consts::TAU * 1.5).sin() * (1.0 - k);
        let (sx, sy) = squash_stretch(-0.11 * (1.0 - k).powi(2) + nod * 0.03);
        BotPose3D {
            bob_y: 0.6 * (1.0 - rec.clamp(0.0, 1.15)) + nod * 0.25,
            lean_x: lerp(0.25, 0.0, ease_out_cubic(k)),
            sx,
            sy,
            yaw: lerp(0.20, -0.15, ease_out_cubic(k)),
            pitch: lerp(0.06, 0.08, k) + nod * 0.14,
            roll: lerp(0.08, 0.0, ease_out_cubic(k)),
            gaze_x: lerp(0.15, 0.0, ease_out_cubic(k)),
            gaze_y: nod * 0.2,
            eye_scale: 1.0,
            hair_dx: lerp(-0.35, 0.0, ease_out_cubic(k)),
            hair_dy: 0.65 * (1.0 - k).powi(2),
            stache_wiggle: nod * 0.55,
            fx: (1.0 - k).powi(2) * 0.45,
            cheer: 0.75,
        }
    }
}

/// Created Bot Idle (4.0 s): gentle living breathing, soft curious 3D look-around, natural blinks.
fn eval_bot_idle_pose(time: f32) -> BotPose3D {
    let u = time.rem_euclid(4.0) / 4.0;
    let breath = (time * 1.75).sin();
    let lag = ((time - 0.16) * 1.75).sin();
    let (sx, sy) = squash_stretch(breath * 0.034);

    let mut yaw = -0.15 + breath * 0.03;
    let mut roll = 0.0;
    let mut gaze_x = 0.0;
    let mut hair_dx = -lag * 0.14;

    if u > 0.35 && u < 0.68 {
        let k = (u - 0.35) / 0.33;
        let turn = (k * std::f32::consts::PI).sin();
        yaw = lerp(-0.15, 0.18, ease_in_out_cubic(turn));
        roll = 0.05 * turn;
        gaze_x = 0.55 * turn;
        hair_dx = -0.35 * turn;
    }

    BotPose3D {
        bob_y: -breath * 0.30,
        lean_x: breath * 0.12,
        sx,
        sy,
        yaw,
        pitch: 0.08 - breath * 0.025,
        roll,
        gaze_x,
        gaze_y: 0.0,
        eye_scale: 1.0,
        hair_dx,
        hair_dy: -lag * 0.20,
        stache_wiggle: breath * 0.15,
        fx: 0.0,
        cheer: 0.45,
    }
}

/// Created Bot Working (2.1 s): buoyant figure-8 3D float with yaw/roll turns & overlapping follow-through.
fn eval_bot_working_pose(time: f32) -> BotPose3D {
    let u = time.rem_euclid(2.1) / 2.1;
    let theta = u * std::f32::consts::TAU;
    let lean_x = theta.sin() * 0.85;
    let hop = theta.sin().abs();
    let bob_y = -hop.powf(0.85) * 1.35 + 0.25;
    let squash = (1.0 - hop).powi(2);
    let (sx, sy) = squash_stretch(-0.085 * squash + 0.065 * hop);
    let vel_x = theta.cos();
    BotPose3D {
        bob_y,
        lean_x,
        sx,
        sy,
        yaw: theta.sin() * 0.34,
        pitch: 0.06 - hop * 0.14,
        roll: -theta.sin() * 0.11,
        gaze_x: lean_x * 0.72,
        gaze_y: -hop * 0.28,
        eye_scale: 1.0 + hop * 0.07,
        hair_dx: -vel_x * 1.15,
        hair_dy: (theta * 2.0 - 0.6).cos() * 0.42,
        stache_wiggle: theta.sin() * 0.45,
        fx: (u * 2.0).fract(),
        cheer: 0.75,
    }
}

/// Awaiting input: attentive head tilt, wide curious eyes, gentle bounce.
fn eval_awaiting_pose(time: f32) -> BotPose3D {
    let b = (time * 2.8).sin();
    let (sx, sy) = squash_stretch(b * 0.045);
    BotPose3D {
        bob_y: -b.abs() * 0.65,
        lean_x: (time * 1.4).sin() * 0.32,
        sx,
        sy,
        yaw: (time * 1.4).sin() * 0.22,
        pitch: -0.06 + b * 0.06,
        roll: (time * 1.4).sin() * 0.09,
        gaze_x: (time * 1.4).sin() * 0.35,
        gaze_y: -0.35,
        eye_scale: 1.14,
        hair_dx: -(time * 1.4).cos() * 0.35,
        hair_dy: -b * 0.25,
        stache_wiggle: b * 0.3,
        fx: 0.0,
        cheer: 0.55,
    }
}

/// Natural irregular blink factor in `0.18..=1.0` with occasional double-blink.
fn compute_blink(time: f32, animate: bool) -> f32 {
    if !animate {
        return 1.0;
    }
    let cycle = time.rem_euclid(4.4);
    let blink_at = |start: f32, dur: f32| -> f32 {
        if cycle > start && cycle < start + dur {
            let u = (cycle - start) / dur;
            1.0 - 0.82 * (u * std::f32::consts::PI).sin()
        } else {
            1.0
        }
    };
    blink_at(3.75, 0.16).min(blink_at(4.02, 0.14))
}

// ---------------------------------------------------------------------------
// Complete 3D Matte-Vinyl Creature Renderer
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn paint_bot_3d(
    bounds: Bounds<Pixels>,
    appearance: BotAppearance,
    is_creator: bool,
    mood: BotMood,
    elapsed: f32,
    phase: f32,
    pop_age: Option<f32>,
    animate: bool,
    t: Tokens,
    window: &mut Window,
) {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    let size_mult = appearance.size.scale();
    // Tight 21.5x21.5 logical viewport so the 3D creature reads big, crisp, and substantial!
    let vp = 21.5;
    let s = (w.min(h) / vp) * size_mult.clamp(0.76, 1.16);
    if s <= 0.05 {
        return;
    }
    let ox = f32::from(bounds.origin.x) + (w - vp * s) * 0.5;
    let oy = f32::from(bounds.origin.y) + (h - vp * s) * 0.5;
    let time = elapsed + phase * 6.2831;
    let dark = is_dark(t.window);

    let pose = evaluate_bot_pose(is_creator, appearance.size, mood, time, pop_age, animate);

    // Exact subagent rounded-box proportions (`hx = 5.9, hy = 5.4, hz = 4.2, radius = 2.4`).
    let hx = 5.9;
    let hy = 5.4;
    let hz = 4.2;
    let radius = 2.4;

    let cam = Cam3D {
        ox,
        oy,
        scale: s,
        cx: vp * 0.5 + pose.lean_x,
        cy: 12.1 + pose.bob_y,
        sx: pose.sx,
        sy: pose.sy,
        yaw: pose.yaw,
        pitch: pose.pitch,
        roll: pose.roll,
    };

    let body_color = bot_body_palette(appearance.color, dark);
    let eye_color = if dark {
        c(0x1112_16FF)
    } else {
        c(0x1C1B_1AFF)
    };
    let catchlight = c(0xFFFF_FFFF).opacity(0.95);

    // 1. Dynamic floor shadow (shrinks & softens as the creature hops!).
    let lift_norm = (-pose.bob_y).max(0.0) / 2.2;
    let shadow_w = (12.4 - lift_norm * 3.6).max(5.5);
    let shadow_h = (2.15 - lift_norm * 0.55).max(1.0);
    let shadow_bounds = Bounds::new(
        point(
            px(ox + (vp * 0.5 + pose.lean_x * 0.55 - shadow_w * 0.5) * s),
            px(oy + 19.2 * s),
        ),
        size(px(shadow_w * s), px(shadow_h * s)),
    );
    let shadow_alpha = (if dark { 0.36 } else { 0.16 }) * (1.0 - lift_norm * 0.42);
    window.paint_quad(
        fill(shadow_bounds, c(0x0000_00FF).opacity(shadow_alpha))
            .corner_radii(px(shadow_h * 0.5 * s)),
    );

    // 2. Back layers behind the 3D rounded box (hero cape drape, Einstein back halo tufts, Bob back).
    if appearance.attire == BotAttire::Cape {
        paint_3d_cape_back(&cam, hx, hy, hz, pose.hair_dx, pose.hair_dy, dark, window);
    }
    if appearance.hair == BotHair::Einstein {
        paint_3d_einstein_hair_back(&cam, hx, hy, hz, pose.hair_dx, pose.hair_dy, dark, window);
    } else if appearance.hair == BotHair::Bob {
        paint_3d_bob_back(&cam, hx, hy, hz, dark, window);
    }

    // 3. Far depth-sorted ear.
    let left_ear_z = cam.xform(Vec3::new(-(hx + 0.75), 0.1, 0.0)).z;
    let right_ear_z = cam.xform(Vec3::new(hx + 0.75, 0.1, 0.0)).z;
    if left_ear_z < 0.0 {
        paint_3d_ear(&cam, hx, -1.0, body_color, window);
    }
    if right_ear_z < 0.0 {
        paint_3d_ear(&cam, hx, 1.0, body_color, window);
    }

    // 4. The 3D Matte-Vinyl Rounded-Corner Box body!
    paint_3d_rounded_box(&cam, hx, hy, hz, radius, body_color, dark, window);

    // 5. Near depth-sorted ear(s).
    if left_ear_z >= 0.0 {
        paint_3d_ear(&cam, hx, -1.0, body_color, window);
    }
    if right_ear_z >= 0.0 {
        paint_3d_ear(&cam, hx, 1.0, body_color, window);
    }

    // 6. 3D Attire layer fitted cleanly onto the lower rounded-box contour.
    paint_3d_attire(
        appearance.attire,
        &cam,
        hx,
        hy,
        hz,
        radius,
        pose.hair_dx,
        dark,
        window,
    );

    // 7. Expressive 3D Face projected onto the front plane (`z = hz + 0.14`).
    let face_z = hz + 0.14;
    let blink = compute_blink(time, animate);
    paint_3d_eyes(
        appearance.eyes,
        &cam,
        hx,
        face_z,
        pose.gaze_x,
        pose.gaze_y,
        pose.eye_scale,
        blink,
        eye_color,
        catchlight,
        dark,
        window,
    );

    // Friendly smile (hidden beneath big full mustaches so the face stays clean).
    if !matches!(
        appearance.mustache,
        BotMustache::Einstein | BotMustache::Goatee
    ) {
        paint_3d_smile(&cam, face_z, pose.gaze_x * 0.35, pose.cheer, mood, eye_color, window);
    }

    // 8. 3D Mustache layer floating crisply in front of the mouth plane!
    paint_3d_mustache(
        appearance.mustache,
        &cam,
        face_z + 0.18,
        pose.stache_wiggle,
        dark,
        window,
    );

    // 9. Front 3D Hair / Headwear layer (depth-sorted over the forehead & temples!).
    paint_3d_hair_front(
        appearance.hair,
        &cam,
        hx,
        hy,
        hz,
        body_color,
        pose.hair_dx,
        pose.hair_dy,
        dark,
        window,
    );

    // 10. Secondary Action FX (Disney appeal!): eureka star on Dr. Keelee / pop sparkle / awaiting pip.
    if animate && pose.fx > 0.14 {
        let star_col = if is_creator {
            c(0xF5C8_4CFF)
        } else {
            adjust_lightness(c(appearance.color.accent_rgba()), 0.18)
        };
        let star_pt = cam.pt(Vec3::new(
            if is_creator { 3.6 } else { 0.0 },
            -hy - 3.0 - pose.fx * 1.2,
            0.4,
        ));
        paint_four_point_star(
            star_pt,
            (1.35 + pose.fx * 1.35) * s,
            star_col.opacity(pose.fx.clamp(0.0, 1.0) * 0.92),
            window,
        );
        if is_creator && pose.fx > 0.35 {
            // Secondary tiny companion sparkle!
            let spark2 = cam.pt(Vec3::new(-4.8, -hy - 2.2 - pose.fx * 0.7, 0.2));
            paint_four_point_star(
                spark2,
                (0.75 + pose.fx * 0.65) * s,
                c(0x7AE5_D8FF).opacity(pose.fx * 0.8),
                window,
            );
        }
    }

    if mood == BotMood::Awaiting {
        let pulse = if animate {
            0.5 + 0.5 * (time * 3.8).sin().abs()
        } else {
            0.9
        };
        let pip = cam.pt(Vec3::new(hx + 0.9, -hy - 0.6, hz));
        let d = 3.4 * s;
        window.paint_quad(
            fill(
                Bounds::new(point(pip.x - px(d * 0.5), pip.y - px(d * 0.5)), size(px(d), px(d))),
                c(t.warning).opacity(pulse),
            )
            .corner_radii(px(d * 0.5)),
        );
    }
}

// ---------------------------------------------------------------------------
// 3D Geometry Primitives (shared style with `subagent_entity.rs`)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn paint_rounded_plane(
    cam: &Cam3D,
    hx: f32,
    hy: f32,
    radius: f32,
    z_plane: f32,
    color: Hsla,
    window: &mut Window,
) {
    let r = radius.min(hx).min(hy);
    let corners = [
        (hx - r, -hy + r, -std::f32::consts::FRAC_PI_2, 0.0),
        (hx - r, hy - r, 0.0, std::f32::consts::FRAC_PI_2),
        (-hx + r, hy - r, std::f32::consts::FRAC_PI_2, std::f32::consts::PI),
        (-hx + r, -hy + r, std::f32::consts::PI, 1.5 * std::f32::consts::PI),
    ];
    let mut pb = PathBuilder::fill();
    let mut started = false;
    const STEPS: usize = 4;
    for (cx, cy, a0, a1) in corners {
        for i in 0..=STEPS {
            let a = lerp(a0, a1, i as f32 / STEPS as f32);
            let pt = cam.pt(Vec3::new(cx + r * a.cos(), cy + r * a.sin(), z_plane));
            if !started {
                pb.move_to(pt);
                started = true;
            } else {
                pb.line_to(pt);
            }
        }
    }
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
}

/// Paint a lower slice of the rounded box (`y` from `y_top` down to `+hy`), matching
/// the exact rounded bottom corners so 3D attire hugs the creature organically!
#[allow(clippy::too_many_arguments)]
fn paint_lower_box_wrap(
    cam: &Cam3D,
    hx: f32,
    hy: f32,
    radius: f32,
    y_top: f32,
    z_plane: f32,
    color: Hsla,
    window: &mut Window,
) {
    let r = radius.min(hx).min((hy - y_top).max(0.5));
    let mut pb = PathBuilder::fill();
    pb.move_to(cam.pt(Vec3::new(-hx, y_top, z_plane)));
    pb.line_to(cam.pt(Vec3::new(hx, y_top, z_plane)));
    // Right-bottom rounded corner
    const STEPS: usize = 4;
    for i in 0..=STEPS {
        let a = lerp(0.0, std::f32::consts::FRAC_PI_2, i as f32 / STEPS as f32);
        pb.line_to(cam.pt(Vec3::new(
            (hx - r) + r * a.cos(),
            (hy - r) + r * a.sin(),
            z_plane,
        )));
    }
    // Left-bottom rounded corner
    for i in 0..=STEPS {
        let a = lerp(
            std::f32::consts::FRAC_PI_2,
            std::f32::consts::PI,
            i as f32 / STEPS as f32,
        );
        pb.line_to(cam.pt(Vec3::new(
            (-hx + r) + r * a.cos(),
            (hy - r) + r * a.sin(),
            z_plane,
        )));
    }
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_3d_rounded_box(
    cam: &Cam3D,
    hx: f32,
    hy: f32,
    hz: f32,
    radius: f32,
    base: Hsla,
    dark_theme: bool,
    window: &mut Window,
) {
    const SLICES: usize = 7;
    for i in 0..SLICES {
        let t = i as f32 / (SLICES - 1) as f32;
        let z = lerp(-hz, hz * 0.88, t);
        let inset = if i == 0 { 0.35 } else { 0.0 };
        let shade = lerp(-0.18, -0.07, t);
        let slice_col = adjust_lightness(base, shade);
        paint_rounded_plane(
            cam,
            hx - inset,
            hy - inset,
            (radius - inset * 0.5).max(0.5),
            z,
            slice_col,
            window,
        );
    }

    if cam.pitch > 0.01 {
        let top_vis = (cam.pitch * 3.5).clamp(0.0, 1.0);
        let top_col =
            adjust_lightness(base, if dark_theme { 0.12 } else { 0.10 }).opacity(0.65 * top_vis);
        let mut pb = PathBuilder::fill();
        let ix = hx - radius * 0.45;
        let iz = hz - 0.5;
        let pts = [
            Vec3::new(-ix, -hy + 0.05, -iz),
            Vec3::new(ix, -hy + 0.05, -iz),
            Vec3::new(ix, -hy + 0.05, iz),
            Vec3::new(-ix, -hy + 0.05, iz),
        ];
        pb.move_to(cam.pt(pts[0]));
        for &v in &pts[1..] {
            pb.line_to(cam.pt(v));
        }
        pb.close();
        if let Ok(path) = pb.build() {
            window.paint_path(path, top_col);
        }
    }

    let front_col = adjust_lightness(base, if dark_theme { 0.04 } else { 0.03 });
    paint_rounded_plane(cam, hx - 0.15, hy - 0.15, radius, hz, front_col, window);

    // Top-left vinyl specular sheen.
    let sheen = adjust_lightness(base, if dark_theme { 0.18 } else { 0.16 }).opacity(0.38);
    let mut pb = PathBuilder::fill();
    let sy0 = -hy + 0.85;
    let sy1 = -hy + 2.35;
    let sx0 = -hx + radius * 0.75;
    let sx1 = hx - radius * 0.75;
    pb.move_to(cam.pt(Vec3::new(sx0, sy0, hz + 0.04)));
    pb.line_to(cam.pt(Vec3::new(sx1, sy0, hz + 0.04)));
    pb.line_to(cam.pt(Vec3::new(sx1 - 0.4, sy1, hz + 0.04)));
    pb.line_to(cam.pt(Vec3::new(sx0 + 0.4, sy1, hz + 0.04)));
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, sheen);
    }
}

fn paint_3d_sphere(
    cam: &Cam3D,
    center: Vec3,
    radius: f32,
    base: Hsla,
    dark_theme: bool,
    window: &mut Window,
) {
    let (pt, persp, _) = cam.project(center);
    let r = radius * persp * cam.scale;
    if r <= 0.3 {
        return;
    }
    let shadow = adjust_lightness(base, -0.12);
    window.paint_quad(
        fill(
            Bounds::new(point(pt.x - px(r), pt.y - px(r)), size(px(r * 2.0), px(r * 2.0))),
            shadow,
        )
        .corner_radii(px(r)),
    );
    let r_lit = r * 0.86;
    window.paint_quad(
        fill(
            Bounds::new(
                point(pt.x - px(r_lit + r * 0.08), pt.y - px(r_lit + r * 0.10)),
                size(px(r_lit * 2.0), px(r_lit * 2.0)),
            ),
            base,
        )
        .corner_radii(px(r_lit)),
    );
    let r_spec = (r * 0.30).max(0.7);
    let spec = adjust_lightness(base, if dark_theme { 0.24 } else { 0.20 }).opacity(0.78);
    window.paint_quad(
        fill(
            Bounds::new(
                point(pt.x - px(r * 0.36 + r_spec), pt.y - px(r * 0.36 + r_spec)),
                size(px(r_spec * 2.0), px(r_spec * 2.0)),
            ),
            spec,
        )
        .corner_radii(px(r_spec)),
    );
}

fn paint_3d_ear(cam: &Cam3D, hx: f32, side: f32, base: Hsla, window: &mut Window) {
    let ear_col = adjust_lightness(base, -0.10);
    let ear_hi = adjust_lightness(base, 0.04);
    let (pt, persp, _) = cam.project(Vec3::new(side * (hx + 0.75), 0.1, 0.0));
    let rw = 1.9 * persp * cam.scale;
    let rh = 3.4 * persp * cam.scale;
    window.paint_quad(
        fill(
            Bounds::new(point(pt.x - px(rw * 0.5), pt.y - px(rh * 0.5)), size(px(rw), px(rh))),
            ear_col,
        )
        .corner_radii(px(rw * 0.5)),
    );
    let iw = rw * 0.62;
    let ih = rh * 0.65;
    window.paint_quad(
        fill(
            Bounds::new(
                point(pt.x - px(iw * 0.5), pt.y - px(ih * 0.5 + rh * 0.08)),
                size(px(iw), px(ih)),
            ),
            ear_hi,
        )
        .corner_radii(px(iw * 0.5)),
    );
}

// ---------------------------------------------------------------------------
// 3D Attire (Doctor Coat + Stethoscope, Hoodie, Suit, Overalls, Cape, Sweater)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn paint_3d_cape_back(
    cam: &Cam3D,
    hx: f32,
    hy: f32,
    hz: f32,
    sway_x: f32,
    sway_y: f32,
    dark: bool,
    window: &mut Window,
) {
    let cape_col = c(if dark { 0xC83A_4AFF } else { 0xB82E_3EFF });
    let cape_sh = adjust_lightness(cape_col, -0.14);
    let mut pb = PathBuilder::fill();
    pb.move_to(cam.pt(Vec3::new(-hx + 0.4, 1.5, -hz * 0.4)));
    pb.line_to(cam.pt(Vec3::new(hx - 0.4, 1.5, -hz * 0.4)));
    pb.line_to(cam.pt(Vec3::new(hx + 2.4 + sway_x * 0.7, hy + 1.8 + sway_y * 0.3, -hz - 1.2)));
    pb.line_to(cam.pt(Vec3::new(sway_x * 0.8, hy + 2.2 + sway_y * 0.3, -hz - 1.5)));
    pb.line_to(cam.pt(Vec3::new(-hx - 2.4 + sway_x * 0.7, hy + 1.8 + sway_y * 0.3, -hz - 1.2)));
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, cape_sh);
    }
    let mut pb2 = PathBuilder::fill();
    pb2.move_to(cam.pt(Vec3::new(-hx + 0.6, 1.8, -hz * 0.2)));
    pb2.line_to(cam.pt(Vec3::new(hx - 0.6, 1.8, -hz * 0.2)));
    pb2.line_to(cam.pt(Vec3::new(hx + 1.8 + sway_x * 0.5, hy + 1.2 + sway_y * 0.2, -hz - 0.6)));
    pb2.line_to(cam.pt(Vec3::new(-hx - 1.8 + sway_x * 0.5, hy + 1.2 + sway_y * 0.2, -hz - 0.6)));
    pb2.close();
    if let Ok(path) = pb2.build() {
        window.paint_path(path, cape_col);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_3d_attire(
    attire: BotAttire,
    cam: &Cam3D,
    hx: f32,
    hy: f32,
    hz: f32,
    radius: f32,
    sway_x: f32,
    dark: bool,
    window: &mut Window,
) {
    match attire {
        BotAttire::Naked => {}
        BotAttire::Doctor => {
            // 3D Crisp White Lab Coat wrapping the lower half of the matte-vinyl cube!
            let coat_sh = c(if dark { 0xC8D2_E0FF } else { 0xD3DCE8FF });
            let coat_mid = c(if dark { 0xE6ECF5FF } else { 0xEDF2F8FF });
            let coat_hi = c(0xFAFC_FFFF);
            let shirt_col = c(0x2866_78FF);

            let y_top = 2.05;
            // Extruded coat thickness slices hugging the 3D box
            for (z, col) in [
                (-hz * 0.55, coat_sh),
                (0.0, adjust_lightness(coat_mid, -0.05)),
                (hz * 0.65, coat_mid),
                (hz + 0.14, coat_hi),
            ] {
                paint_lower_box_wrap(cam, hx + 0.16, hy + 0.14, radius, y_top, z, col, window);
            }

            let fz = hz + 0.22;
            // Dark slate-teal undershirt V-neck
            let mut shirt = PathBuilder::fill();
            shirt.move_to(cam.pt(Vec3::new(-2.15, y_top - 0.05, fz)));
            shirt.line_to(cam.pt(Vec3::new(2.15, y_top - 0.05, fz)));
            shirt.line_to(cam.pt(Vec3::new(0.0, hy - 0.55, fz)));
            shirt.close();
            if let Ok(path) = shirt.build() {
                window.paint_path(path, shirt_col);
            }

            // Sculpted notched white coat lapels
            let lapel_col = c(0xFFFF_FFFF);
            let lapel_edge = c(0x94A3_B8FF).opacity(0.8);
            for side in [-1.0_f32, 1.0_f32] {
                let mut lp = PathBuilder::fill();
                lp.move_to(cam.pt(Vec3::new(side * 2.15, y_top - 0.08, fz + 0.06)));
                lp.line_to(cam.pt(Vec3::new(side * 3.25, y_top + 0.85, fz + 0.06)));
                lp.line_to(cam.pt(Vec3::new(side * 1.95, y_top + 1.85, fz + 0.06)));
                lp.line_to(cam.pt(Vec3::new(side * 0.12, hy - 0.65, fz + 0.06)));
                lp.close();
                if let Ok(path) = lp.build() {
                    window.paint_path(path, lapel_col);
                }
            }
            // Center coat seam
            let mut seam = PathBuilder::stroke(px((0.7 * cam.scale).max(0.65)));
            seam.move_to(cam.pt(Vec3::new(0.0, hy - 0.65, fz + 0.06)));
            seam.line_to(cam.pt(Vec3::new(0.0, hy + 0.08, fz + 0.06)));
            if let Ok(path) = seam.build() {
                window.paint_path(path, lapel_edge);
            }

            // Breast pocket + blue & crimson lab pens!
            let mut pock = PathBuilder::fill();
            pock.move_to(cam.pt(Vec3::new(2.1, 3.35, fz + 0.08)));
            pock.line_to(cam.pt(Vec3::new(4.3, 3.35, fz + 0.08)));
            pock.line_to(cam.pt(Vec3::new(4.1, 4.75, fz + 0.08)));
            pock.line_to(cam.pt(Vec3::new(2.2, 4.75, fz + 0.08)));
            pock.close();
            if let Ok(path) = pock.build() {
                window.paint_path(path, c(0xDCE4_F0FF));
            }
            let pen_sw = (0.85 * cam.scale).max(0.75);
            for (px_x, col) in [(2.8, c(0x2563_EBFF)), (3.55, c(0xE11D_48FF))] {
                let mut pen = PathBuilder::stroke(px(pen_sw));
                pen.move_to(cam.pt(Vec3::new(px_x, 2.75, fz + 0.10)));
                pen.line_to(cam.pt(Vec3::new(px_x, 3.55, fz + 0.10)));
                if let Ok(path) = pen.build() {
                    window.paint_path(path, col);
                }
            }

            // Iconic 3D Stethoscope draped around Dr. Keelee's neck with overlapping sway!
            let tube_sw = (0.72 * cam.scale).max(1.0);
            let tube_col = c(0x3341_55FF);
            let mut steth = PathBuilder::stroke(px(tube_sw));
            steth.move_to(cam.pt(Vec3::new(-3.2, y_top - 0.15, fz + 0.12)));
            steth.line_to(cam.pt(Vec3::new(-2.6 + sway_x * 0.15, 3.65, fz + 0.20)));
            steth.line_to(cam.pt(Vec3::new(-0.7 + sway_x * 0.20, 4.15, fz + 0.22)));
            steth.line_to(cam.pt(Vec3::new(1.5 + sway_x * 0.15, 3.60, fz + 0.20)));
            steth.line_to(cam.pt(Vec3::new(2.7, y_top - 0.15, fz + 0.12)));
            if let Ok(path) = steth.build() {
                window.paint_path(path, tube_col);
            }
            // Sleek metallic stethoscope chestpiece disc (outer silver rim + dark diaphragm)
            let (cpt, cpersp, _) = cam.project(Vec3::new(-2.35 + sway_x * 0.22, 4.25, fz + 0.30));
            let cr = 0.88 * cpersp * cam.scale;
            window.paint_quad(
                fill(
                    Bounds::new(point(cpt.x - px(cr), cpt.y - px(cr)), size(px(cr * 2.0), px(cr * 2.0))),
                    c(0x94A3_B8FF),
                )
                .corner_radii(px(cr)),
            );
            let ir = cr * 0.62;
            window.paint_quad(
                fill(
                    Bounds::new(point(cpt.x - px(ir), cpt.y - px(ir)), size(px(ir * 2.0), px(ir * 2.0))),
                    c(0x3341_55FF),
                )
                .corner_radii(px(ir)),
            );
        }
        BotAttire::Hoodie => {
            let base = c(if dark { 0x3D46_5BFF } else { 0x343C_4FFF });
            let y_top = 2.05;
            for (z, delta) in [(-hz * 0.5, -0.12), (0.0, -0.05), (hz + 0.14, 0.04)] {
                paint_lower_box_wrap(
                    cam,
                    hx + 0.18,
                    hy + 0.14,
                    radius,
                    y_top,
                    z,
                    adjust_lightness(base, delta),
                    window,
                );
            }
            let fz = hz + 0.22;
            // Drawstrings with slight sway
            let sw = (0.85 * cam.scale).max(0.75);
            for side in [-1.0_f32, 1.0_f32] {
                let mut ds = PathBuilder::stroke(px(sw));
                ds.move_to(cam.pt(Vec3::new(side * 1.35, y_top + 0.1, fz)));
                ds.line_to(cam.pt(Vec3::new(side * 1.45 + sway_x * 0.2, y_top + 2.15, fz + 0.08)));
                if let Ok(path) = ds.build() {
                    window.paint_path(path, c(0xE2E8_F0FF));
                }
            }
            // Kangaroo pocket
            let mut kp = PathBuilder::fill();
            kp.move_to(cam.pt(Vec3::new(-2.7, 3.8, fz)));
            kp.line_to(cam.pt(Vec3::new(2.7, 3.8, fz)));
            kp.line_to(cam.pt(Vec3::new(2.2, 5.15, fz)));
            kp.line_to(cam.pt(Vec3::new(-2.2, 5.15, fz)));
            kp.close();
            if let Ok(path) = kp.build() {
                window.paint_path(path, adjust_lightness(base, -0.08));
            }
        }
        BotAttire::Suit => {
            let jacket = c(if dark { 0x2631_44FF } else { 0x1E29_3BFF });
            let y_top = 2.05;
            for (z, delta) in [(-hz * 0.5, -0.10), (hz + 0.14, 0.04)] {
                paint_lower_box_wrap(
                    cam,
                    hx + 0.16,
                    hy + 0.14,
                    radius,
                    y_top,
                    z,
                    adjust_lightness(jacket, delta),
                    window,
                );
            }
            let fz = hz + 0.22;
            // Crisp white dress shirt V
            let mut sh = PathBuilder::fill();
            sh.move_to(cam.pt(Vec3::new(-2.0, y_top - 0.05, fz)));
            sh.line_to(cam.pt(Vec3::new(2.0, y_top - 0.05, fz)));
            sh.line_to(cam.pt(Vec3::new(0.0, hy - 0.2, fz)));
            sh.close();
            if let Ok(path) = sh.build() {
                window.paint_path(path, c(0xFFFF_FFFF));
            }
            // Crimson tie
            let mut tie = PathBuilder::fill();
            tie.move_to(cam.pt(Vec3::new(-0.55, y_top + 0.1, fz + 0.06)));
            tie.line_to(cam.pt(Vec3::new(0.55, y_top + 0.1, fz + 0.06)));
            tie.line_to(cam.pt(Vec3::new(0.75, hy - 0.9, fz + 0.06)));
            tie.line_to(cam.pt(Vec3::new(0.0, hy - 0.1, fz + 0.06)));
            tie.line_to(cam.pt(Vec3::new(-0.75, hy - 0.9, fz + 0.06)));
            tie.close();
            if let Ok(path) = tie.build() {
                window.paint_path(path, c(0xDC26_26FF));
            }
        }
        BotAttire::Overalls => {
            let denim = c(0x3B6E_A5FF);
            let y_top = 3.05;
            for (z, delta) in [(-hz * 0.45, -0.12), (hz + 0.14, 0.04)] {
                paint_lower_box_wrap(
                    cam,
                    hx + 0.15,
                    hy + 0.14,
                    radius,
                    y_top,
                    z,
                    adjust_lightness(denim, delta),
                    window,
                );
            }
            let fz = hz + 0.20;
            // Straps + brass buttons
            for side in [-1.0_f32, 1.0_f32] {
                let mut strap = PathBuilder::fill();
                strap.move_to(cam.pt(Vec3::new(side * 3.1, 1.85, fz)));
                strap.line_to(cam.pt(Vec3::new(side * 1.9, 1.85, fz)));
                strap.line_to(cam.pt(Vec3::new(side * 1.9, y_top + 0.4, fz)));
                strap.line_to(cam.pt(Vec3::new(side * 3.1, y_top + 0.4, fz)));
                strap.close();
                if let Ok(path) = strap.build() {
                    window.paint_path(path, adjust_lightness(denim, 0.05));
                }
                paint_3d_sphere(
                    cam,
                    Vec3::new(side * 2.5, y_top + 0.35, fz + 0.15),
                    0.62,
                    c(0xFBBF_24FF),
                    dark,
                    window,
                );
            }
        }
        BotAttire::Cape => {
            // Front cape ribbon + glowing golden clasp
            let fz = hz + 0.22;
            let mut rib = PathBuilder::stroke(px((1.35 * cam.scale).max(1.1)));
            rib.move_to(cam.pt(Vec3::new(-hx + 0.2, 2.25, fz - 0.15)));
            rib.line_to(cam.pt(Vec3::new(0.0, 2.85, fz + 0.12)));
            rib.line_to(cam.pt(Vec3::new(hx - 0.2, 2.25, fz - 0.15)));
            if let Ok(path) = rib.build() {
                window.paint_path(path, c(0xC83A_4AFF));
            }
            paint_3d_sphere(cam, Vec3::new(0.0, 2.85, fz + 0.25), 0.95, c(0xF5B9_38FF), dark, window);
        }
        BotAttire::Sweater => {
            let knit = c(0xF0E2_CCFF);
            let y_top = 2.05;
            for (z, delta) in [(-hz * 0.5, -0.10), (hz + 0.14, 0.03)] {
                paint_lower_box_wrap(
                    cam,
                    hx + 0.16,
                    hy + 0.14,
                    radius,
                    y_top,
                    z,
                    adjust_lightness(knit, delta),
                    window,
                );
            }
            // Warm terracotta knit stripe across front
            let fz = hz + 0.22;
            let mut stripe = PathBuilder::stroke(px((1.65 * cam.scale).max(1.2)));
            stripe.move_to(cam.pt(Vec3::new(-hx + 0.2, 3.65, fz)));
            stripe.line_to(cam.pt(Vec3::new(hx - 0.2, 3.65, fz)));
            if let Ok(path) = stripe.build() {
                window.paint_path(path, c(0xD977_57FF));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3D Eyes & Smile
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn paint_3d_eyes(
    eyes: BotEyes,
    cam: &Cam3D,
    hx: f32,
    face_z: f32,
    gaze_x: f32,
    gaze_y: f32,
    eye_scale: f32,
    blink: f32,
    eye_color: Hsla,
    catchlight: Hsla,
    dark: bool,
    window: &mut Window,
) {
    let eye_y = -0.75 + gaze_y * 0.85;
    let xs = [-2.35 + gaze_x * 0.75, 2.35 + gaze_x * 0.75];
    let yaw_foreshorten = cam.yaw.cos().max(0.65);

    match eyes {
        BotEyes::Dot | BotEyes::Specs => {
            let eh = (2.35 * eye_scale * blink).max(0.65);
            let ew = 1.95;
            for cx in xs {
                let (pt, persp, _) = cam.project(Vec3::new(cx, eye_y, face_z));
                let pw = ew * yaw_foreshorten * persp * cam.scale;
                let ph = eh * persp * cam.scale;
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(pt.x - px(pw * 0.5), pt.y - px(ph * 0.5)),
                            size(px(pw), px(ph)),
                        ),
                        eye_color,
                    )
                    .corner_radii(px(pw.min(ph) * 0.5)),
                );
                if blink > 0.65 {
                    let cd = (0.76 * persp * cam.scale).max(0.85);
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                point(pt.x - px(pw * 0.24 + cd * 0.4), pt.y - px(ph * 0.24 + cd * 0.4)),
                                size(px(cd), px(cd)),
                            ),
                            catchlight,
                        )
                        .corner_radii(px(cd * 0.5)),
                    );
                }
            }
            if eyes == BotEyes::Specs {
                let frame_col = c(if dark { 0xE8C8_74FF } else { 0x3D46_56FF });
                paint_3d_round_glasses(cam, hx, face_z + 0.62, eye_y, frame_col, window);
            }
        }
        BotEyes::Wide => {
            for cx in xs {
                let (pt, persp, _) = cam.project(Vec3::new(cx, eye_y, face_z));
                let rw = 2.75 * yaw_foreshorten * persp * cam.scale;
                let rh = (3.05 * eye_scale * blink).max(0.85) * persp * cam.scale;
                window.paint_quad(
                    fill(
                        Bounds::new(point(pt.x - px(rw * 0.5), pt.y - px(rh * 0.5)), size(px(rw), px(rh))),
                        c(0xFFFF_FFFF),
                    )
                    .corner_radii(px(rw.min(rh) * 0.5)),
                );
                let pw = rw * 0.66;
                let ph = rh * 0.68;
                window.paint_quad(
                    fill(
                        Bounds::new(point(pt.x - px(pw * 0.5), pt.y - px(ph * 0.5)), size(px(pw), px(ph))),
                        eye_color,
                    )
                    .corner_radii(px(pw.min(ph) * 0.5)),
                );
                if blink > 0.65 {
                    let cd = (0.85 * persp * cam.scale).max(0.9);
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                point(pt.x - px(pw * 0.25 + cd * 0.35), pt.y - px(ph * 0.25 + cd * 0.35)),
                                size(px(cd), px(cd)),
                            ),
                            catchlight,
                        )
                        .corner_radii(px(cd * 0.5)),
                    );
                }
            }
        }
        BotEyes::Sleepy => {
            let sw = (1.15 * cam.scale).max(1.0);
            for cx in xs {
                let (pt, persp, _) = cam.project(Vec3::new(cx, eye_y + 0.25, face_z));
                let pw = 1.95 * yaw_foreshorten * persp * cam.scale;
                let ph = 1.35 * blink * persp * cam.scale;
                window.paint_quad(
                    fill(
                        Bounds::new(point(pt.x - px(pw * 0.5), pt.y - px(ph * 0.5)), size(px(pw), px(ph))),
                        eye_color,
                    )
                    .corner_radii(px(pw.min(ph) * 0.5)),
                );
                // Half-lidded top line
                let mut lid = PathBuilder::stroke(px(sw));
                lid.move_to(cam.pt(Vec3::new(cx - 1.25, eye_y - 0.35, face_z + 0.06)));
                lid.line_to(cam.pt(Vec3::new(cx + 1.25, eye_y - 0.20, face_z + 0.06)));
                if let Ok(path) = lid.build() {
                    window.paint_path(path, eye_color);
                }
            }
        }
        BotEyes::Shades => {
            let z = face_z + 0.68;
            let dark_glass = c(0x1619_22FF);
            let glint = c(0xFFFF_FFFF).opacity(0.42);
            for cx in [-2.45_f32, 2.45_f32] {
                let mut pb = PathBuilder::fill();
                pb.move_to(cam.pt(Vec3::new(cx - 2.15, eye_y - 1.45, z)));
                pb.line_to(cam.pt(Vec3::new(cx + 2.15, eye_y - 1.45, z)));
                pb.line_to(cam.pt(Vec3::new(cx + 1.85, eye_y + 1.55, z)));
                pb.line_to(cam.pt(Vec3::new(cx - 1.85, eye_y + 1.55, z)));
                pb.close();
                if let Ok(path) = pb.build() {
                    window.paint_path(path, dark_glass);
                }
                let mut gl = PathBuilder::stroke(px((0.95 * cam.scale).max(0.85)));
                gl.move_to(cam.pt(Vec3::new(cx - 1.15, eye_y + 0.6, z + 0.06)));
                gl.line_to(cam.pt(Vec3::new(cx + 0.35, eye_y - 0.85, z + 0.06)));
                if let Ok(path) = gl.build() {
                    window.paint_path(path, glint);
                }
            }
            let sw = (1.15 * cam.scale).max(1.0);
            let mut bridge = PathBuilder::stroke(px(sw));
            bridge.move_to(cam.pt(Vec3::new(-0.45, eye_y - 0.75, z)));
            bridge.line_to(cam.pt(Vec3::new(0.45, eye_y - 0.75, z)));
            if let Ok(path) = bridge.build() {
                window.paint_path(path, dark_glass);
            }
            for (ex, sx) in [(-4.5, -hx - 0.1), (4.5, hx + 0.1)] {
                let mut arm = PathBuilder::stroke(px(sw * 0.85));
                arm.move_to(cam.pt(Vec3::new(ex, eye_y - 0.7, z)));
                arm.line_to(cam.pt(Vec3::new(sx, eye_y - 0.8, 0.4)));
                if let Ok(path) = arm.build() {
                    window.paint_path(path, dark_glass);
                }
            }
        }
        BotEyes::Wink => {
            // Left eye open + glossy, right eye playful happy wink arc (^)!
            let (pt, persp, _) = cam.project(Vec3::new(xs[0], eye_y, face_z));
            let pw = 2.0 * yaw_foreshorten * persp * cam.scale;
            let ph = (2.45 * eye_scale * blink).max(0.65) * persp * cam.scale;
            window.paint_quad(
                fill(
                    Bounds::new(point(pt.x - px(pw * 0.5), pt.y - px(ph * 0.5)), size(px(pw), px(ph))),
                    eye_color,
                )
                .corner_radii(px(pw.min(ph) * 0.5)),
            );
            if blink > 0.65 {
                let cd = (0.78 * persp * cam.scale).max(0.85);
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(pt.x - px(pw * 0.24 + cd * 0.4), pt.y - px(ph * 0.24 + cd * 0.4)),
                            size(px(cd), px(cd)),
                        ),
                        catchlight,
                    )
                    .corner_radii(px(cd * 0.5)),
                );
            }
            let sw = (1.2 * cam.scale).max(1.0);
            let rx = xs[1];
            let mut wk = PathBuilder::stroke(px(sw));
            wk.move_to(cam.pt(Vec3::new(rx - 1.1, eye_y + 0.25, face_z)));
            wk.line_to(cam.pt(Vec3::new(rx, eye_y - 0.65, face_z)));
            wk.line_to(cam.pt(Vec3::new(rx + 1.1, eye_y + 0.25, face_z)));
            if let Ok(path) = wk.build() {
                window.paint_path(path, eye_color);
            }
        }
    }
}

/// Sleek 3D Round Wire Lab Spectacles (crisp thin rims + subtle lens glint + temple arms).
fn paint_3d_round_glasses(
    cam: &Cam3D,
    hx: f32,
    z: f32,
    y: f32,
    tint: Hsla,
    window: &mut Window,
) {
    let r = 2.25;
    let sw = (0.52 * cam.scale).max(1.0);
    let lx = -2.42;
    let rx = 2.42;

    for cx in [lx, rx] {
        let mut fill_pb = PathBuilder::fill();
        let mut rim_pb = PathBuilder::stroke(px(sw));
        const SEG: usize = 20;
        for i in 0..=SEG {
            let a = (i as f32 / SEG as f32) * std::f32::consts::TAU;
            let pt = cam.pt(Vec3::new(cx + r * a.cos(), y + r * a.sin(), z));
            if i == 0 {
                fill_pb.move_to(pt);
                rim_pb.move_to(pt);
            } else {
                fill_pb.line_to(pt);
                rim_pb.line_to(pt);
            }
        }
        fill_pb.close();
        if let Ok(path) = fill_pb.build() {
            window.paint_path(path, c(0xFFFF_FFFF).opacity(0.11));
        }
        if let Ok(path) = rim_pb.build() {
            window.paint_path(path, tint);
        }
        // Subtle diagonal glass glint
        let mut glint = PathBuilder::stroke(px((0.36 * cam.scale).max(0.75)));
        glint.move_to(cam.pt(Vec3::new(cx - 1.15, y - 0.55, z + 0.04)));
        glint.line_to(cam.pt(Vec3::new(cx - 0.45, y - 1.25, z + 0.04)));
        if let Ok(path) = glint.build() {
            window.paint_path(path, c(0xFFFF_FFFF).opacity(0.45));
        }
    }

    let mut bridge = PathBuilder::stroke(px(sw * 0.92));
    bridge.move_to(cam.pt(Vec3::new(lx + r - 0.08, y - 0.12, z)));
    bridge.line_to(cam.pt(Vec3::new(rx - r + 0.08, y - 0.12, z)));
    if let Ok(path) = bridge.build() {
        window.paint_path(path, tint);
    }

    for (edge_x, side_x) in [(lx - r, -hx - 0.1), (rx + r, hx + 0.1)] {
        let mut arm = PathBuilder::stroke(px(sw * 0.85));
        arm.move_to(cam.pt(Vec3::new(edge_x, y - 0.22, z)));
        arm.line_to(cam.pt(Vec3::new(side_x, y - 0.48, 0.4)));
        if let Ok(path) = arm.build() {
            window.paint_path(path, tint.opacity(0.88));
        }
    }
}

fn paint_3d_smile(
    cam: &Cam3D,
    face_z: f32,
    dx: f32,
    cheer: f32,
    mood: BotMood,
    eye_color: Hsla,
    window: &mut Window,
) {
    let my = 1.45;
    if mood == BotMood::Awaiting {
        let (pt, persp, _) = cam.project(Vec3::new(dx, my, face_z));
        let d = 1.35 * persp * cam.scale;
        window.paint_quad(
            fill(
                Bounds::new(point(pt.x - px(d * 0.5), pt.y - px(d * 0.5)), size(px(d), px(d))),
                eye_color.opacity(0.82),
            )
            .corner_radii(px(d * 0.5)),
        );
        return;
    }
    let sw = (0.95 * cam.scale).max(0.85);
    let dip = 0.35 + cheer * 0.35;
    let mut pb = PathBuilder::stroke(px(sw));
    pb.move_to(cam.pt(Vec3::new(dx - 1.05, my - 0.15, face_z)));
    pb.line_to(cam.pt(Vec3::new(dx, my + dip, face_z)));
    pb.line_to(cam.pt(Vec3::new(dx + 1.05, my - 0.15, face_z)));
    if let Ok(path) = pb.build() {
        window.paint_path(path, eye_color.opacity(0.82));
    }
}

// ---------------------------------------------------------------------------
// 3D Mustache Layer (with Einstein Fluffy Walrus Mustache & Wiggle!)
// ---------------------------------------------------------------------------

fn paint_3d_mustache(
    mustache: BotMustache,
    cam: &Cam3D,
    z: f32,
    wiggle: f32,
    dark: bool,
    window: &mut Window,
) {
    let my = 1.05;
    let dy = wiggle * 0.22;
    match mustache {
        BotMustache::None => {}
        BotMustache::Einstein => {
            // Sculpted, sweeping bushy Albert Einstein walrus mustache (feathery bristle contour, zero spheres!).
            let base = c(if dark { 0xF0F3_F8FF } else { 0xF7F9_FCFF });
            let sh = c(if dark { 0xB8C2_D4FF } else { 0xC4CD_DDFF });
            let bristle = c(if dark { 0x9AA5_B8FF } else { 0xAEB8_C9FF }).opacity(0.62);

            let paint_walrus = |z_off: f32, y_off: f32, col: Hsla, window: &mut Window| {
                let mut pb = PathBuilder::fill();
                let pts = [
                    // Upper arch under nose
                    (0.0, my - 0.18 + y_off),
                    (1.35, my - 0.08 + y_off),
                    (2.75, my + 0.32 + y_off - dy * 0.5),
                    (3.95, my + 0.92 + y_off - dy),
                    // Feathered outer-right bristle tips
                    (4.25, my + 1.55 + y_off - dy),
                    (3.35, my + 1.48 + y_off - dy * 0.8),
                    (2.95, my + 1.92 + y_off - dy * 0.6),
                    (1.95, my + 1.62 + y_off - dy * 0.4),
                    (1.15, my + 1.88 + y_off - dy * 0.2),
                    // Center dip
                    (0.0, my + 1.38 + y_off),
                    // Symmetrical feathered left bristle tips
                    (-1.15, my + 1.88 + y_off + dy * 0.2),
                    (-1.95, my + 1.62 + y_off + dy * 0.4),
                    (-2.95, my + 1.92 + y_off + dy * 0.6),
                    (-3.35, my + 1.48 + y_off + dy * 0.8),
                    (-4.25, my + 1.55 + y_off + dy),
                    (-3.95, my + 0.92 + y_off + dy),
                    (-2.75, my + 0.32 + y_off + dy * 0.5),
                    (-1.35, my - 0.08 + y_off),
                ];
                pb.move_to(cam.pt(Vec3::new(pts[0].0, pts[0].1, z_off)));
                for &(x, y) in &pts[1..] {
                    pb.line_to(cam.pt(Vec3::new(x, y, z_off)));
                }
                pb.close();
                if let Ok(path) = pb.build() {
                    window.paint_path(path, col);
                }
            };

            // 3D depth shadow layer behind + bright snowy sculpted front layer
            paint_walrus(z - 0.12, 0.24, sh, window);
            paint_walrus(z + 0.14, 0.0, base, window);

            // Delicate vertical bristle texture lines
            let bsw = (0.36 * cam.scale).max(0.65);
            for (x0, x1) in [(-2.3, -2.55), (-1.1, -1.2), (1.1, 1.2), (2.3, 2.55)] {
                let mut l = PathBuilder::stroke(px(bsw));
                l.move_to(cam.pt(Vec3::new(x0, my + 0.45, z + 0.18)));
                l.line_to(cam.pt(Vec3::new(x1, my + 1.35, z + 0.18)));
                if let Ok(path) = l.build() {
                    window.paint_path(path, bristle);
                }
            }
        }
        BotMustache::Handlebar => {
            let col = c(0x3B2B_23FF);
            let sw = (1.35 * cam.scale).max(1.1);
            for side in [-1.0_f32, 1.0_f32] {
                let mut pb = PathBuilder::stroke(px(sw));
                pb.move_to(cam.pt(Vec3::new(0.0, my + 0.15, z)));
                pb.line_to(cam.pt(Vec3::new(side * 1.8, my + 0.65, z + 0.08)));
                pb.line_to(cam.pt(Vec3::new(side * 3.25, my + 0.55 + side * dy, z + 0.12)));
                pb.line_to(cam.pt(Vec3::new(side * 3.55, my - 0.25 + side * dy, z + 0.14)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, col);
                }
            }
        }
        BotMustache::Chevron => {
            let col = c(0x3B2B_23FF);
            let mut pb = PathBuilder::fill();
            pb.move_to(cam.pt(Vec3::new(0.0, my - 0.15, z)));
            pb.line_to(cam.pt(Vec3::new(-2.85, my + 0.75 + dy, z)));
            pb.line_to(cam.pt(Vec3::new(-2.55, my + 1.55 + dy, z)));
            pb.line_to(cam.pt(Vec3::new(0.0, my + 0.85, z)));
            pb.line_to(cam.pt(Vec3::new(2.55, my + 1.55 - dy, z)));
            pb.line_to(cam.pt(Vec3::new(2.85, my + 0.75 - dy, z)));
            pb.close();
            if let Ok(path) = pb.build() {
                window.paint_path(path, col);
            }
        }
        BotMustache::Pencil => {
            let col = c(0x2821_1FFF);
            let sw = (0.85 * cam.scale).max(0.75);
            for side in [-1.0_f32, 1.0_f32] {
                let mut pb = PathBuilder::stroke(px(sw));
                pb.move_to(cam.pt(Vec3::new(side * 0.25, my + 0.2, z)));
                pb.line_to(cam.pt(Vec3::new(side * 2.45, my + 0.55 + side * dy * 0.5, z)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, col);
                }
            }
        }
        BotMustache::Goatee => {
            let col = c(0x3B2B_23FF);
            let sw = (1.15 * cam.scale).max(0.95);
            let mut m = PathBuilder::stroke(px(sw));
            m.move_to(cam.pt(Vec3::new(-2.2, my + 0.55, z)));
            m.line_to(cam.pt(Vec3::new(0.0, my + 0.15, z)));
            m.line_to(cam.pt(Vec3::new(2.2, my + 0.55, z)));
            if let Ok(path) = m.build() {
                window.paint_path(path, col);
            }
            paint_3d_sphere(cam, Vec3::new(0.0, my + 1.95, z + 0.08), 1.05, col, dark, window);
        }
    }
}

// ---------------------------------------------------------------------------
// 3D Hair & Headwear Layer (with Wild Depth-Sorted Einstein Cloud Tufts!)
// ---------------------------------------------------------------------------

/// Iconic Albert Einstein wild swept-outward side manes (back 3D volume layer).
/// Leaves the high forehead open so the silhouette reads unmistakably as Einstein,
/// NOT a round wig — sweeping spiky-feathered white tufts bursting outward on both sides!
#[allow(clippy::too_many_arguments)]
fn paint_3d_einstein_hair_back(
    cam: &Cam3D,
    hx: f32,
    hy: f32,
    hz: f32,
    lag_x: f32,
    lag_y: f32,
    dark: bool,
    window: &mut Window,
) {
    let silver_back = c(if dark { 0xC6CF_E0FF } else { 0xD2DAE8FF });
    let silver_mid = c(if dark { 0xDFE5_F0FF } else { 0xE8EDF6FF });

    for side in [-1.0_f32, 1.0_f32] {
        // Back extruded shadow wing bursting wildly outward & upward from the temple!
        for (z_off, scale_out, col) in [
            (-hz * 0.55, 1.06, silver_back),
            (-0.4, 1.0, silver_mid),
        ] {
            let mut pb = PathBuilder::fill();
            let pts = [
                // Root along upper temple & crown edge
                (side * (hx - 2.6), -hy - 0.1),
                // High wild wispy flare #1 (shooting diagonally up-out)
                (side * (hx + 0.9 * scale_out) + lag_x, -hy - 2.65 * scale_out + lag_y),
                (side * (hx + 0.6 * scale_out) + lag_x * 0.7, -hy - 0.85 + lag_y * 0.6),
                // Dramatic horizontal Einstein wing #2 (widest wild burst!)
                (side * (hx + 4.05 * scale_out) + lag_x * 1.15, -hy - 1.35 * scale_out + lag_y * 0.85),
                (side * (hx + 2.15 * scale_out) + lag_x * 0.85, -hy + 0.65 + lag_y * 0.5),
                // Swept mid-temple wing #3
                (side * (hx + 4.35 * scale_out) + lag_x * 1.1, -hy + 1.45 + lag_y * 0.65),
                (side * (hx + 2.05 * scale_out) + lag_x * 0.75, -hy + 2.65 + lag_y * 0.4),
                // Lower fluffy cheek tuft #4 (flaring out around the ear)
                (side * (hx + 3.45 * scale_out) + lag_x * 0.95, -hy + 4.05 + lag_y * 0.45),
                (side * (hx + 0.65), -hy + 4.95),
                // Tuck back behind the side wall
                (side * (hx - 1.2), -hy + 4.2),
            ];
            pb.move_to(cam.pt(Vec3::new(pts[0].0, pts[0].1, z_off)));
            for &(x, y) in &pts[1..] {
                pb.line_to(cam.pt(Vec3::new(x, y, z_off)));
            }
            pb.close();
            if let Ok(path) = pb.build() {
                window.paint_path(path, col);
            }
        }
    }

    // Subtle wispy back crown tufts bridging behind the receding forehead
    let mut crown = PathBuilder::fill();
    let cpts = [
        (-3.8, -hy + 0.2),
        (-2.9 + lag_x * 0.8, -hy - 2.05 + lag_y),
        (-1.3 + lag_x * 0.5, -hy - 0.65 + lag_y * 0.5),
        (0.2 + lag_x * 0.9, -hy - 2.35 + lag_y),
        (1.5 + lag_x * 0.6, -hy - 0.75 + lag_y * 0.5),
        (3.1 + lag_x * 0.8, -hy - 1.95 + lag_y),
        (3.8, -hy + 0.2),
    ];
    crown.move_to(cam.pt(Vec3::new(cpts[0].0, cpts[0].1, -hz * 0.35)));
    for &(x, y) in &cpts[1..] {
        crown.line_to(cam.pt(Vec3::new(x, y, -hz * 0.35)));
    }
    crown.close();
    if let Ok(path) = crown.build() {
        window.paint_path(path, silver_mid);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_3d_bob_back(
    cam: &Cam3D,
    hx: f32,
    hy: f32,
    hz: f32,
    _dark: bool,
    window: &mut Window,
) {
    let col = c(0x2F25_2DFF);
    paint_rounded_plane(cam, hx + 1.05, hy * 0.85, 3.0, -hz * 0.45, col, window);
}

#[allow(clippy::too_many_arguments)]
fn paint_3d_hair_front(
    hair: BotHair,
    cam: &Cam3D,
    hx: f32,
    hy: f32,
    hz: f32,
    body_color: Hsla,
    lag_x: f32,
    lag_y: f32,
    dark: bool,
    window: &mut Window,
) {
    match hair {
        BotHair::None => {}
        BotHair::Einstein => {
            // Front bright snowy-white swept Einstein temple wings + wispy stray strands!
            // Keeps the center forehead open (classic Einstein high brow) while flaring wildly at the sides.
            let white = c(if dark { 0xF2F5_FAFF } else { 0xFAFBFDFF });
            let strand = c(if dark { 0xBCC6_D8FF } else { 0xCBD3_E2FF }).opacity(0.68);
            let ssw = (0.38 * cam.scale).max(0.7);

            // Depth-sort left & right temple wings so yaw turns render cleanly!
            let lz = cam.xform(Vec3::new(-hx - 1.2, -hy + 1.5, 1.4)).z;
            let rz = cam.xform(Vec3::new(hx + 1.2, -hy + 1.5, 1.4)).z;
            let sides = if lz < rz { [-1.0_f32, 1.0_f32] } else { [1.0_f32, -1.0_f32] };

            for side in sides {
                let z_front = 1.65;
                let mut wing = PathBuilder::fill();
                let pts = [
                    // Temple hairline hugging upper corner (leaving forehead clear!)
                    (side * (hx - 1.85), -hy + 0.15),
                    // Swept upper-outer spike
                    (side * (hx + 1.85) + lag_x, -hy - 1.85 + lag_y),
                    (side * (hx + 1.05) + lag_x * 0.7, -hy - 0.15 + lag_y * 0.6),
                    // Bold mid-temple swept tuft
                    (side * (hx + 3.45) + lag_x * 1.15, -hy + 0.35 + lag_y * 0.75),
                    (side * (hx + 1.45) + lag_x * 0.75, -hy + 1.85 + lag_y * 0.45),
                    // Lower outward flick above ear
                    (side * (hx + 2.85) + lag_x * 0.95, -hy + 3.15 + lag_y * 0.4),
                    (side * (hx + 0.15), -hy + 3.65),
                    (side * (hx - 0.45), -hy + 1.65),
                ];
                wing.move_to(cam.pt(Vec3::new(pts[0].0, pts[0].1, z_front)));
                for &(x, y) in &pts[1..] {
                    wing.line_to(cam.pt(Vec3::new(x, y, z_front)));
                }
                wing.close();
                if let Ok(path) = wing.build() {
                    window.paint_path(path, white);
                }

                // Crisp internal hair-sweep line for feathered Einstein texture
                let mut line = PathBuilder::stroke(px(ssw));
                line.move_to(cam.pt(Vec3::new(side * (hx - 0.4), -hy + 0.95, z_front + 0.08)));
                line.line_to(cam.pt(Vec3::new(
                    side * (hx + 2.25) + lag_x * 0.9,
                    -hy + 0.55 + lag_y * 0.6,
                    z_front + 0.08,
                )));
                if let Ok(path) = line.build() {
                    window.paint_path(path, strand);
                }
            }

            // 2 playful electrified stray Einstein hairs curving off the crown!
            let mut wisp1 = PathBuilder::stroke(px((0.46 * cam.scale).max(0.85)));
            wisp1.move_to(cam.pt(Vec3::new(-1.8, -hy + 0.1, 1.2)));
            wisp1.line_to(cam.pt(Vec3::new(-2.4 + lag_x * 0.9, -hy - 1.45 + lag_y, 1.3)));
            wisp1.line_to(cam.pt(Vec3::new(-3.2 + lag_x * 1.2, -hy - 2.15 + lag_y * 1.1, 1.35)));
            if let Ok(path) = wisp1.build() {
                window.paint_path(path, white);
            }
            let mut wisp2 = PathBuilder::stroke(px((0.46 * cam.scale).max(0.85)));
            wisp2.move_to(cam.pt(Vec3::new(2.1, -hy + 0.1, 1.2)));
            wisp2.line_to(cam.pt(Vec3::new(2.9 + lag_x * 0.9, -hy - 1.35 + lag_y, 1.3)));
            wisp2.line_to(cam.pt(Vec3::new(3.7 + lag_x * 1.2, -hy - 1.85 + lag_y * 1.1, 1.35)));
            if let Ok(path) = wisp2.build() {
                window.paint_path(path, white);
            }
        }
        BotHair::Tuft => {
            // Adorable 3D curved sprout / cowlick with overlapping bounce!
            let stem = adjust_lightness(body_color, -0.10);
            let leaf = adjust_lightness(body_color, 0.08);
            let mut pb = PathBuilder::stroke(px((1.35 * cam.scale).max(1.0)));
            pb.move_to(cam.pt(Vec3::new(0.0, -hy + 0.2, 0.2)));
            pb.line_to(cam.pt(Vec3::new(0.4 + lag_x * 0.6, -hy - 2.3 + lag_y, 0.35)));
            if let Ok(path) = pb.build() {
                window.paint_path(path, stem);
            }
            paint_3d_sphere(
                cam,
                Vec3::new(-0.8 + lag_x, -hy - 2.75 + lag_y, 0.45),
                1.35,
                leaf,
                dark,
                window,
            );
            paint_3d_sphere(
                cam,
                Vec3::new(1.3 + lag_x, -hy - 2.35 + lag_y, 0.45),
                1.15,
                leaf,
                dark,
                window,
            );
        }
        BotHair::Spiky => {
            let col = c(0x2B30_3CFF);
            let hi = c(0x434B_5EFF);
            let peaks = [
                (-3.4, -hy - 2.4, 0.4),
                (-1.1, -hy - 3.3, 0.9),
                (1.4, -hy - 3.1, 0.9),
                (3.5, -hy - 2.2, 0.4),
            ];
            for (px_x, px_y, pz) in peaks {
                let mut sp = PathBuilder::fill();
                sp.move_to(cam.pt(Vec3::new(px_x - 1.5, -hy + 0.45, pz)));
                sp.line_to(cam.pt(Vec3::new(px_x + lag_x * 0.6, px_y + lag_y * 0.7, pz + 0.35)));
                sp.line_to(cam.pt(Vec3::new(px_x + 1.5, -hy + 0.45, pz)));
                sp.close();
                if let Ok(path) = sp.build() {
                    window.paint_path(path, col);
                }
            }
            // Crisp highlight ridge
            let mut h = PathBuilder::fill();
            h.move_to(cam.pt(Vec3::new(-1.8, -hy + 0.3, 1.3)));
            h.line_to(cam.pt(Vec3::new(-1.1 + lag_x * 0.6, -hy - 2.9 + lag_y * 0.7, 1.45)));
            h.line_to(cam.pt(Vec3::new(-0.5, -hy + 0.3, 1.3)));
            h.close();
            if let Ok(path) = h.build() {
                window.paint_path(path, hi.opacity(0.65));
            }
        }
        BotHair::Bob => {
            let col = c(0x3B2E_36FF);
            let hi = c(0x5A46_52FF);
            let mut pb = PathBuilder::fill();
            pb.move_to(cam.pt(Vec3::new(-hx - 0.35, -hy + 2.4, hz + 0.25)));
            pb.line_to(cam.pt(Vec3::new(-hx + 0.2, -hy - 0.55, hz * 0.4)));
            pb.line_to(cam.pt(Vec3::new(0.0, -hy - 1.15, hz * 0.4)));
            pb.line_to(cam.pt(Vec3::new(hx - 0.2, -hy - 0.55, hz * 0.4)));
            pb.line_to(cam.pt(Vec3::new(hx + 0.35, -hy + 2.4, hz + 0.25)));
            pb.line_to(cam.pt(Vec3::new(1.8, -hy + 1.4, hz + 0.30)));
            pb.line_to(cam.pt(Vec3::new(-2.2, -hy + 1.8, hz + 0.30)));
            pb.close();
            if let Ok(path) = pb.build() {
                window.paint_path(path, col);
            }
            let mut sh = PathBuilder::stroke(px((0.9 * cam.scale).max(0.8)));
            sh.move_to(cam.pt(Vec3::new(-2.8, -hy + 0.1, hz + 0.32)));
            sh.line_to(cam.pt(Vec3::new(1.8, -hy - 0.15, hz + 0.32)));
            if let Ok(path) = sh.build() {
                window.paint_path(path, hi.opacity(0.7));
            }
        }
        BotHair::Mohawk => {
            let base = c(0xE11D_48FF);
            let fin_z = [-2.2, -0.6, 1.0, 2.4];
            for (ix, &z) in fin_z.iter().enumerate() {
                let h = if ix == 1 || ix == 2 { 3.5 } else { 2.7 };
                paint_3d_sphere(
                    cam,
                    Vec3::new(lag_x * 0.65, -hy - h * 0.52 + lag_y * 0.6, z),
                    1.55,
                    adjust_lightness(base, (ix as f32 - 1.5) * 0.04),
                    dark,
                    window,
                );
            }
        }
        BotHair::Beanie => {
            let knit = c(0xDE58_47FF);
            let cuff = c(0xC243_34FF);
            let base_y = -hy + 0.45 + lag_y * 0.35;
            // Smooth 3D knitted dome (matching hard-hat extrusion style)
            for (z_off, r_scale, delta) in [
                (-hz * 0.55, 0.95, -0.14),
                (0.0, 1.0, -0.04),
                (hz * 0.55, 0.97, 0.05),
            ] {
                let mut pb = PathBuilder::fill();
                const SEG: usize = 14;
                for i in 0..=SEG {
                    let a = (i as f32 / SEG as f32) * std::f32::consts::PI;
                    let x = lag_x * 0.35 + (hx + 0.25) * r_scale * a.cos();
                    let y = base_y - 3.8 * r_scale * a.sin().max(0.0).powf(0.86);
                    let pt = cam.pt(Vec3::new(x, y, z_off));
                    if i == 0 {
                        pb.move_to(pt);
                    } else {
                        pb.line_to(pt);
                    }
                }
                pb.close();
                if let Ok(path) = pb.build() {
                    window.paint_path(path, adjust_lightness(knit, delta));
                }
            }
            // Folded ribbed cuff
            let mut cf = PathBuilder::fill();
            cf.move_to(cam.pt(Vec3::new(-hx - 0.45, base_y - 0.75, hz + 0.22)));
            cf.line_to(cam.pt(Vec3::new(hx + 0.45, base_y - 0.75, hz + 0.22)));
            cf.line_to(cam.pt(Vec3::new(hx + 0.45, base_y + 0.95, hz + 0.25)));
            cf.line_to(cam.pt(Vec3::new(-hx - 0.45, base_y + 0.95, hz + 0.25)));
            cf.close();
            if let Ok(path) = cf.build() {
                window.paint_path(path, cuff);
            }
            // Bouncy 3D pom-pom sphere on top!
            paint_3d_sphere(
                cam,
                Vec3::new(lag_x * 0.95, base_y - 4.35 + lag_y * 0.9, 0.2),
                1.55,
                c(0xF5E7_D8FF),
                dark,
                window,
            );
        }
    }
}

fn paint_four_point_star(center: Point<Pixels>, radius: f32, color: Hsla, window: &mut Window) {
    let r = radius;
    let inner = radius * 0.32;
    let mut pb = PathBuilder::fill();
    pb.move_to(point(center.x, center.y - px(r)));
    pb.line_to(point(center.x + px(inner), center.y - px(inner)));
    pb.line_to(point(center.x + px(r), center.y));
    pb.line_to(point(center.x + px(inner), center.y + px(inner)));
    pb.line_to(point(center.x, center.y + px(r)));
    pb.line_to(point(center.x - px(inner), center.y + px(inner)));
    pb.line_to(point(center.x - px(r), center.y));
    pb.line_to(point(center.x - px(inner), center.y - px(inner)));
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
}

// ---------------------------------------------------------------------------
// Curated Matte-Vinyl Palettes (matching `subagent_entity.rs`)
// ---------------------------------------------------------------------------

fn bot_body_palette(color: BotColor, dark: bool) -> Hsla {
    let rgba: u32 = match (color, dark) {
        (BotColor::Clay, true) => 0xDFAF_8EFF,
        (BotColor::Clay, false) => 0xD39D_7AFF,
        (BotColor::Coral, true) => 0xDC73_58FF,
        (BotColor::Coral, false) => 0xCC60_44FF,
        (BotColor::Amber, true) => 0xE5B2_4EFF,
        (BotColor::Amber, false) => 0xD29C_36FF,
        (BotColor::Sage, true) => 0x52B3_88FF,
        (BotColor::Sage, false) => 0x3E9E_74FF,
        (BotColor::Mint, true) => 0x45A8_A6FF,
        (BotColor::Mint, false) => 0x3496_94FF,
        (BotColor::Sky, true) => 0x6184_E8FF,
        (BotColor::Sky, false) => 0x4B6D_D4FF,
        (BotColor::Indigo, true) => 0x7D7F_E6FF,
        (BotColor::Indigo, false) => 0x6769_D4FF,
        (BotColor::Plum, true) => 0xA87A_D8FF,
        (BotColor::Plum, false) => 0x9262_C4FF,
    };
    c(rgba)
}

fn is_dark(rgba: u32) -> bool {
    let r = ((rgba >> 24) & 0xFF) as f32;
    let g = ((rgba >> 16) & 0xFF) as f32;
    let b = ((rgba >> 8) & 0xFF) as f32;
    (0.299 * r + 0.587 * g + 0.114 * b) < 128.0
}

fn adjust_lightness(mut color: Hsla, delta: f32) -> Hsla {
    color.l = (color.l + delta).clamp(0.05, 0.95);
    color
}

fn fnv1a(s: &str) -> u32 {
    let mut hash: u32 = 2_166_136_261;
    for b in s.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squash_and_stretch_preserves_volume() {
        for stretch in [-0.15, -0.07, 0.0, 0.07, 0.14] {
            let (sx, sy) = squash_stretch(stretch);
            assert!((sx * sy - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn dr_keelee_working_cycle_hits_anticipation_eureka_and_settle() {
        let crouch = evaluate_bot_pose(true, BotSize::Medium, BotMood::Working, 2.7 * 0.40, None, true);
        assert!(crouch.sy < 0.95, "expected anticipation crouch squash");
        let eureka = evaluate_bot_pose(true, BotSize::Medium, BotMood::Working, 2.7 * 0.55, None, true);
        assert!(eureka.bob_y < -0.8, "expected upward eureka arc hop");
        assert!(eureka.fx > 0.5, "expected eureka star burst");
    }

    #[test]
    fn customizer_pop_triggers_3d_showoff_spin_and_settles() {
        let pop = evaluate_bot_pose(false, BotSize::Medium, BotMood::Idle, 0.0, Some(0.28), true);
        assert!(pop.bob_y < -0.8, "expected buoyant customizer pop");
        assert!(pop.fx > 0.4, "expected celebratory sparkle");
    }

    #[test]
    fn random_appearance_produces_valid_combinations() {
        let a = random_appearance(1);
        let b = random_appearance(2);
        assert_ne!(a, b);
    }
}
