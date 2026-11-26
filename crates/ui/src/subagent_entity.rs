//! 3D-projected animated rounded-box creatures for Keel subagents.
//!
//! Each subagent is rendered as a miniature 3D matte-vinyl rounded cube with
//! real perspective projection, 3D yaw/pitch/roll rotations, directional
//! face shading, depth-sorted ears and role props, and Disney's 12 Principles
//! of Animation (`dylantarre/animation-principles`):
//!
//! - **Squash & Stretch**: volume-preserving deformation (`sx * sy ≈ 1.0`).
//! - **Anticipation**: wind-up crouch/tilt before strikes, hops, and eureka pops.
//! - **Pose-to-Pose + Arc**: curved 3D trajectories with cubic/elastic easing.
//! - **Follow-Through & Overlapping Action**: hard hat recoil lag, 3D binocular
//!   barrels leading the scan, and 3D springy antenna whip.
//! - **Timing & Hit-Stop**: impact beats on worker strikes, dwell pauses while
//!   exploring/pondering, and a slow 3.2 s breathing cadence while sleeping.
//! - **Secondary Action & Appeal**: work sparks, discovery/eureka stars, signal
//!   ripples, floating `z`s, specular top bevels, and eye catchlights.

use std::time::Instant;

use gpui::{
    AnyElement, Bounds, Hsla, PathBuilder, Pixels, Point, Window, fill, point, prelude::*, px,
    size,
};
use keel_theme::Tokens;

use crate::style::{c, zpx};

/// Visual archetype derived from the subagent's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentRole {
    /// Teal rounded-box scout with 3D brass binoculars + yaw-scanning & double-take hop.
    Explore,
    /// Cobalt-blue rounded-box architect with 3D gold round glasses + pondering & eureka pop.
    Plan,
    /// Terracotta rounded-box builder with 3D yellow hard hat + strike/hit-stop & sparks.
    Worker,
    /// Sage-emerald rounded-box all-rounder with 3D springy antenna + signal ripple.
    General,
    /// Custom agent: deterministic colored rounded-box + 3D crest from name hash.
    Custom(u8),
}

impl SubagentRole {
    pub fn from_name(raw: &str) -> Self {
        let trimmed = raw.trim().to_ascii_lowercase();
        let core = trimmed.strip_suffix(" agent").unwrap_or(&trimmed).trim();
        match core {
            "explore" | "explorer" | "research" | "researcher" | "search" => Self::Explore,
            "plan" | "planner" | "architect" => Self::Plan,
            "worker" | "builder" | "coder" | "implementer" | "dev" => Self::Worker,
            "" | "general" | "subagent" => Self::General,
            other => Self::Custom((fnv1a(other) % 4) as u8),
        }
    }
}

/// Lifecycle state reflected in the creature's expression and motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityState {
    Running,
    Blocked,
    Done,
    Error,
    Cancelled,
}

impl EntityState {
    pub fn from_status(status: &str) -> Self {
        match status.trim().to_ascii_lowercase().as_str() {
            "" | "running" | "in_progress" => Self::Running,
            "blocked" | "awaiting_input" | "waiting" => Self::Blocked,
            "error" | "failed" | "errored" => Self::Error,
            "cancelled" | "canceled" | "interrupted" | "aborted" => Self::Cancelled,
            _ => Self::Done,
        }
    }

    pub const fn is_animated(self) -> bool {
        matches!(self, Self::Running | Self::Blocked | Self::Done)
    }
}

/// Render a 3D rounded-box subagent creature at `logical_size` (scaled via `zpx`).
pub fn render_entity(
    subagent_id: &str,
    name: &str,
    state: EntityState,
    logical_size: f32,
    clock_epoch: Instant,
    t: Tokens,
    reduced: bool,
) -> AnyElement {
    render_entity_transformed(subagent_id, name, state, logical_size, 1.0, 0.0, clock_epoch, t, reduced)
}

/// Render a 3D rounded-box subagent creature with an additional visual `scale`
/// and vertical `dy_px` offset inside its box — ideal for Emil-style `0.85 -> 1.0`
/// enter/exit transitions without triggering layout shifts.
#[allow(clippy::too_many_arguments)]
pub fn render_entity_transformed(
    subagent_id: &str,
    name: &str,
    state: EntityState,
    logical_size: f32,
    visual_scale: f32,
    dy_px: f32,
    clock_epoch: Instant,
    t: Tokens,
    reduced: bool,
) -> AnyElement {
    let role = SubagentRole::from_name(name);
    let phase = (fnv1a(subagent_id) % 1000) as f32 / 1000.0;
    let animate = state.is_animated() && !reduced;
    let dim = zpx(logical_size);

    gpui::div()
        .flex_none()
        .size(dim)
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
                    paint_creature_3d(
                        bounds,
                        role,
                        state,
                        elapsed,
                        phase,
                        animate,
                        visual_scale,
                        dy_px,
                        t,
                        window,
                    );
                },
            )
            .size_full(),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// 3D Vector & Camera Projection
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

/// 3D transform for the creature in 24×24 logical viewport space.
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

    /// Apply squash/stretch and 3D roll → pitch → yaw rotation in creature-local space.
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

    /// Project a creature-local 3D point onto screen `Point<Pixels>` plus its perspective scale & depth.
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
// Easing & Disney Pose-to-Pose Kinematics
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

fn ease_in_quad(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t
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

fn squash_stretch(stretch: f32) -> (f32, f32) {
    let sy = (1.0 + stretch).clamp(0.80, 1.22);
    let sx = 1.0 / sy;
    (sx, sy)
}

#[derive(Clone, Copy, Debug)]
struct Pose3D {
    bob_y: f32,
    lean_x: f32,
    sx: f32,
    sy: f32,
    /// 3D head/box orientation in radians.
    yaw: f32,
    pitch: f32,
    roll: f32,
    /// Eye gaze offset on the front face.
    gaze_x: f32,
    gaze_y: f32,
    eye_scale: f32,
    /// Secondary prop follow-through offset in local 3D units.
    prop_dx: f32,
    prop_dy: f32,
    /// Secondary-effect burst `0..=1` (sparks, discovery/eureka star, ping, sleep z's).
    fx: f32,
    /// Secondary stage value (thought dots / sleep breath).
    aux: f32,
}

impl Default for Pose3D {
    fn default() -> Self {
        Self {
            bob_y: 0.0,
            lean_x: 0.0,
            sx: 1.0,
            sy: 1.0,
            // Subtle 3/4 resting turn so the 3D rounded-box depth is always readable!
            yaw: -0.14,
            pitch: 0.08,
            roll: 0.0,
            gaze_x: 0.0,
            gaze_y: 0.0,
            eye_scale: 1.0,
            prop_dx: 0.0,
            prop_dy: 0.0,
            fx: 0.0,
            aux: 0.0,
        }
    }
}

fn evaluate_pose(role: SubagentRole, state: EntityState, time: f32, animate: bool) -> Pose3D {
    if state == EntityState::Done {
        return eval_sleeping_pose(role, time, animate);
    }
    if !animate {
        return Pose3D::default();
    }
    if state == EntityState::Blocked {
        let b = (time * 2.8).sin();
        let (sx, sy) = squash_stretch(b * 0.045);
        return Pose3D {
            bob_y: -b.abs() * 0.6,
            lean_x: (time * 1.4).sin() * 0.3,
            sx,
            sy,
            yaw: (time * 1.4).sin() * 0.22,
            pitch: -0.06 + b * 0.06,
            roll: (time * 1.4).sin() * 0.08,
            gaze_x: (time * 1.4).sin() * 0.35,
            gaze_y: -0.35,
            eye_scale: 1.12,
            ..Pose3D::default()
        };
    }

    match role {
        SubagentRole::Worker => eval_worker_pose(time),
        SubagentRole::Explore => eval_explore_pose(time),
        SubagentRole::Plan => eval_plan_pose(time),
        SubagentRole::General | SubagentRole::Custom(_) => eval_general_pose(time),
    }
}

/// Worker: Anticipation crouch & pull-back → 3D forward pitch strike → Hit-stop squash + sparks → Hard hat recoil.
fn eval_worker_pose(time: f32) -> Pose3D {
    let u = time.rem_euclid(1.38) / 1.38;
    if u < 0.24 {
        let k = ease_in_out_cubic(u / 0.24);
        let (sx, sy) = squash_stretch(-0.12 * k);
        Pose3D {
            bob_y: 0.9 * k,
            lean_x: -0.85 * k,
            sx,
            sy,
            yaw: lerp(-0.12, -0.32, k),
            pitch: lerp(0.06, -0.20, k),
            roll: -0.10 * k,
            gaze_x: -0.3 * k,
            gaze_y: -0.2 * k,
            eye_scale: 0.95,
            prop_dx: -0.2 * k,
            prop_dy: 0.2 * k,
            ..Pose3D::default()
        }
    } else if u < 0.37 {
        let k = ease_in_quad((u - 0.24) / 0.13);
        let (sx, sy) = squash_stretch(lerp(-0.12, 0.11, k));
        Pose3D {
            bob_y: lerp(0.9, -0.3, k) - arc(k) * 1.25,
            lean_x: lerp(-0.85, 0.95, k),
            sx,
            sy,
            yaw: lerp(-0.32, 0.26, k),
            pitch: lerp(-0.20, 0.28, k),
            roll: lerp(-0.10, 0.12, k),
            gaze_x: lerp(-0.3, 0.65, k),
            gaze_y: lerp(-0.2, 0.3, k),
            eye_scale: 0.9,
            prop_dx: lerp(-0.2, 0.1, k),
            prop_dy: -arc(k) * 0.8,
            ..Pose3D::default()
        }
    } else if u < 0.47 {
        let k = (u - 0.37) / 0.10;
        let (sx, sy) = squash_stretch(-0.15 * (1.0 - k * 0.25));
        Pose3D {
            bob_y: 0.95 * (1.0 - k * 0.15),
            lean_x: 0.95 * (1.0 - k * 0.1),
            sx,
            sy,
            yaw: 0.26,
            pitch: 0.28,
            roll: 0.12,
            gaze_x: 0.65,
            gaze_y: 0.35,
            eye_scale: 0.84,
            prop_dx: 0.25 * k,
            prop_dy: -0.95 * arc(k),
            fx: k * 0.35 + 0.05,
            ..Pose3D::default()
        }
    } else if u < 0.80 {
        let k = (u - 0.47) / 0.33;
        let rec = ease_out_back(k);
        let hat_rec = ease_out_cubic(((k - 0.12) / 0.88).clamp(0.0, 1.0));
        let (sx, sy) = squash_stretch(lerp(-0.12, 0.0, rec.clamp(0.0, 1.1)));
        Pose3D {
            bob_y: lerp(0.85, 0.0, rec),
            lean_x: lerp(0.85, 0.0, ease_out_cubic(k)),
            sx,
            sy,
            yaw: lerp(0.26, -0.12, ease_out_cubic(k)),
            pitch: lerp(0.28, 0.06, ease_out_cubic(k)),
            roll: lerp(0.12, 0.0, ease_out_cubic(k)),
            gaze_x: lerp(0.65, 0.0, ease_out_cubic(k)),
            gaze_y: lerp(0.35, 0.1, ease_out_cubic(k)),
            eye_scale: lerp(0.84, 1.0, ease_out_cubic(k)),
            prop_dx: lerp(0.25, 0.0, hat_rec),
            prop_dy: lerp(-0.8, 0.0, hat_rec) - arc(k) * 0.4,
            fx: 0.4 + k * 0.6,
            ..Pose3D::default()
        }
    } else {
        let k = (u - 0.80) / 0.20;
        let breath = (k * std::f32::consts::PI).sin();
        let (sx, sy) = squash_stretch(breath * 0.03);
        Pose3D {
            bob_y: -breath * 0.25,
            sx,
            sy,
            yaw: -0.12,
            pitch: 0.06,
            gaze_y: 0.1,
            ..Pose3D::default()
        }
    }
}

/// Explore: True 3D yaw sweep left → dwell & peer → excited double-take hop → 3D yaw sweep right → settle.
fn eval_explore_pose(time: f32) -> Pose3D {
    let u = time.rem_euclid(2.5) / 2.5;
    if u < 0.24 {
        let k = ease_in_out_cubic(u / 0.24);
        let (sx, sy) = squash_stretch(arc(k) * 0.05);
        Pose3D {
            bob_y: -arc(k) * 0.75 - 0.2 * k,
            lean_x: lerp(0.0, -1.1, k),
            sx,
            sy,
            yaw: lerp(0.0, -0.44, k),
            pitch: lerp(0.06, 0.02, k),
            roll: -0.08 * k,
            gaze_x: lerp(0.0, -1.0, k),
            prop_dx: lerp(0.0, -0.35, ease_out_cubic(k)),
            prop_dy: -arc(k) * 0.25,
            ..Pose3D::default()
        }
    } else if u < 0.46 {
        let k = (u - 0.24) / 0.22;
        let peer = (k * std::f32::consts::TAU).sin();
        Pose3D {
            bob_y: -0.2 + peer * 0.2,
            lean_x: -1.1 + peer * 0.12,
            yaw: -0.44 + peer * 0.05,
            pitch: 0.02 + peer * 0.05,
            roll: -0.08,
            gaze_x: -1.0 + peer * 0.2,
            prop_dx: -0.35 + peer * 0.12,
            ..Pose3D::default()
        }
    } else if u < 0.62 {
        let k = (u - 0.46) / 0.16;
        let hop = arc(k);
        let (sx, sy) = if k < 0.25 {
            squash_stretch(-0.10 * (k / 0.25))
        } else {
            squash_stretch(0.12 * hop)
        };
        Pose3D {
            bob_y: -0.2 - hop * 1.95,
            lean_x: lerp(-1.1, -0.45, ease_in_out_cubic(k)),
            sx,
            sy,
            yaw: lerp(-0.44, -0.18, ease_in_out_cubic(k)),
            pitch: -0.16 * hop,
            roll: lerp(-0.08, 0.0, k),
            gaze_x: lerp(-1.0, -0.4, k),
            gaze_y: -0.4 * hop,
            eye_scale: 1.15,
            prop_dx: lerp(-0.35, -0.15, k),
            prop_dy: -hop * 0.5,
            fx: hop,
            ..Pose3D::default()
        }
    } else if u < 0.80 {
        let k = ease_in_out_cubic((u - 0.62) / 0.18);
        let (sx, sy) = squash_stretch(arc(k) * 0.06);
        Pose3D {
            bob_y: -arc(k) * 0.9 - 0.2 * k,
            lean_x: lerp(-0.45, 1.1, k),
            sx,
            sy,
            yaw: lerp(-0.18, 0.44, k),
            pitch: 0.02,
            roll: lerp(0.0, 0.08, k),
            gaze_x: lerp(-0.4, 1.0, k),
            prop_dx: lerp(-0.15, 0.35, ease_out_cubic(k)),
            prop_dy: -arc(k) * 0.25,
            ..Pose3D::default()
        }
    } else {
        let k = (u - 0.80) / 0.20;
        let ret = ease_in_out_cubic(((k - 0.45) / 0.55).clamp(0.0, 1.0));
        Pose3D {
            bob_y: lerp(-0.2, 0.0, ret),
            lean_x: lerp(1.1, 0.0, ret),
            yaw: lerp(0.44, 0.0, ret),
            pitch: lerp(0.02, 0.06, ret),
            roll: lerp(0.08, 0.0, ret),
            gaze_x: lerp(1.0, 0.0, ret),
            prop_dx: lerp(0.35, 0.0, ret),
            ..Pose3D::default()
        }
    }
}

/// Plan: Disney pose-to-pose "Think → Idea → Write on Notebook → Settle" cycle (3.0 s).
/// - `aux`: 0.0..=1.0 ink write progress (0.0 while pondering, 0.0..=0.5 line 1, 0.5..=1.0 line 2).
/// - `prop_dx`: pen tip X progress on page (or chin-tap offset while thinking).
/// - `prop_dy`: pen lift (>0 = lowered onto notebook, <0 = lifted to cheek while thinking).
/// - `fx`: eureka star intensity right before writing starts.
fn eval_plan_pose(time: f32) -> Pose3D {
    let u = time.rem_euclid(3.0) / 3.0;
    if u < 0.30 {
        // Phase 1: Ponder up-left, pencil lifted to cheek with gentle thought taps.
        let k = ease_in_out_cubic(u / 0.30);
        let tap = (u * 22.0).sin().abs() * 0.35;
        Pose3D {
            bob_y: -0.35 * k,
            lean_x: -0.5 * k,
            yaw: lerp(-0.10, -0.26, k),
            pitch: lerp(0.06, -0.20, k),
            roll: -0.09 * k,
            gaze_x: -0.95 * k,
            gaze_y: -0.65 * k,
            prop_dx: tap,
            prop_dy: -1.6 * k - tap * 0.4, // pencil raised while thinking
            fx: 0.0,
            aux: 0.0,
            ..Pose3D::default()
        }
    } else if u < 0.42 {
        // Phase 2: Anticipation dip -> crisp Eureka pop before writing!
        let k = (u - 0.30) / 0.12;
        let pop = arc(k);
        let (sx, sy) = if k < 0.3 {
            squash_stretch(-0.08 * (k / 0.3))
        } else {
            squash_stretch(0.10 * pop)
        };
        Pose3D {
            bob_y: -0.35 - pop * 1.15,
            lean_x: lerp(-0.5, 0.0, ease_in_out_cubic(k)),
            sx,
            sy,
            yaw: lerp(-0.26, -0.08, ease_in_out_cubic(k)),
            pitch: lerp(-0.20, -0.04, ease_in_out_cubic(k)),
            roll: lerp(-0.09, 0.0, k),
            gaze_x: lerp(-0.95, 0.0, k),
            gaze_y: lerp(-0.65, -0.15, k),
            eye_scale: 1.0 + 0.16 * pop,
            prop_dx: 0.0,
            prop_dy: lerp(-1.6, -0.4, ease_in_out_cubic(k)),
            fx: pop, // eureka sparkle!
            aux: 0.0,
        }
    } else if u < 0.66 {
        // Phase 3: Pitch head down toward notebook & write Line 1 left -> right with micro scribbles!
        let k = (u - 0.42) / 0.24;
        let stroke = ease_in_out_cubic(k);
        let scribble = (k * std::f32::consts::TAU * 3.0).sin() * 0.28;
        let (sx, sy) = squash_stretch(-0.04 + scribble * 0.05);
        Pose3D {
            bob_y: 0.45 + scribble * 0.3,
            lean_x: lerp(-0.25, 0.35, stroke),
            sx,
            sy,
            yaw: lerp(-0.18, 0.06, stroke),
            pitch: 0.25 + scribble * 0.04,
            roll: lerp(-0.04, 0.05, stroke),
            gaze_x: lerp(-0.85, 0.75, stroke),
            gaze_y: 0.85,
            eye_scale: 0.94,
            prop_dx: stroke,        // 0..1 across line 1
            prop_dy: 1.0 + scribble * 0.35, // pen down on page (line 1)
            fx: 0.0,
            aux: 0.02 + stroke * 0.48,
        }
    } else if u < 0.71 {
        // Quick curved carriage-return arc back to the start of Line 2!
        let k = (u - 0.66) / 0.05;
        let hop = arc(k);
        Pose3D {
            bob_y: 0.35 - hop * 0.4,
            lean_x: lerp(0.35, -0.25, ease_in_out_cubic(k)),
            yaw: lerp(0.06, -0.18, ease_in_out_cubic(k)),
            pitch: 0.23 - hop * 0.05,
            roll: lerp(0.05, -0.04, k),
            gaze_x: lerp(0.75, -0.85, k),
            gaze_y: 0.9,
            prop_dx: lerp(1.0, 0.0, ease_in_out_cubic(k)),
            prop_dy: 1.0 - hop * 0.95, // pen lifts slightly in an arc during return
            fx: 0.0,
            aux: 0.50,
            ..Pose3D::default()
        }
    } else if u < 0.89 {
        // Phase 4: Write Line 2 left -> right!
        let k = (u - 0.71) / 0.18;
        let stroke = ease_in_out_cubic(k);
        let scribble = (k * std::f32::consts::TAU * 2.5).sin() * 0.28;
        let (sx, sy) = squash_stretch(-0.04 + scribble * 0.05);
        Pose3D {
            bob_y: 0.48 + scribble * 0.3,
            lean_x: lerp(-0.25, 0.25, stroke),
            sx,
            sy,
            yaw: lerp(-0.18, 0.02, stroke),
            pitch: 0.27 + scribble * 0.04,
            roll: lerp(-0.04, 0.04, stroke),
            gaze_x: lerp(-0.85, 0.55, stroke),
            gaze_y: 0.92,
            eye_scale: 0.94,
            prop_dx: stroke,
            prop_dy: 2.0 + scribble * 0.35, // pen down on page (line 2)
            fx: 0.0,
            aux: 0.50 + stroke * 0.50,
        }
    } else {
        // Phase 5: Satisfied lift & settle before the next thought.
        let k = ease_out_cubic((u - 0.89) / 0.11);
        let (sx, sy) = squash_stretch(lerp(-0.04, 0.0, k));
        Pose3D {
            bob_y: lerp(0.45, 0.0, k),
            lean_x: lerp(0.25, 0.0, k),
            sx,
            sy,
            yaw: lerp(0.02, -0.10, k),
            pitch: lerp(0.27, 0.06, k),
            roll: lerp(0.04, 0.0, k),
            gaze_x: lerp(0.55, 0.0, k),
            gaze_y: lerp(0.92, 0.0, k),
            prop_dx: 0.5,
            prop_dy: lerp(2.0, -0.2, k),
            fx: 0.0,
            aux: 1.0,
            ..Pose3D::default()
        }
    }
}

/// General / Custom: Buoyant 3D figure-8 float with yaw/roll turn & springy antenna whip.
fn eval_general_pose(time: f32) -> Pose3D {
    let u = time.rem_euclid(2.1) / 2.1;
    let theta = u * std::f32::consts::TAU;
    let lean_x = theta.sin() * 0.9;
    let hop = theta.sin().abs();
    let bob_y = -hop.powf(0.85) * 1.35 + 0.25;
    let squash = (1.0 - hop).powi(2);
    let (sx, sy) = squash_stretch(-0.08 * squash + 0.06 * hop);
    let velocity_x = theta.cos();
    let prop_dx = -velocity_x * 1.6;
    let prop_dy = (theta * 2.0 - 0.6).cos() * 0.35;
    let ping = (u * 2.0).fract();
    Pose3D {
        bob_y,
        lean_x,
        sx,
        sy,
        yaw: theta.sin() * 0.32,
        pitch: 0.06 - hop * 0.12,
        roll: -theta.sin() * 0.11,
        gaze_x: lean_x * 0.7,
        gaze_y: -hop * 0.3,
        eye_scale: 1.0 + hop * 0.06,
        prop_dx,
        prop_dy,
        fx: ping,
        aux: 0.0,
    }
}

/// Cozy sleeping cycle (3.2 s): bowed head, tilted roll, slow breathing squash & stretch, floating `z`s.
fn eval_sleeping_pose(role: SubagentRole, time: f32, animate: bool) -> Pose3D {
    let base_prop = |breath_lag: f32| -> (f32, f32) {
        match role {
            SubagentRole::Worker => (0.0, 0.45 - breath_lag * 0.18),
            SubagentRole::Plan => (0.0, 0.38 - breath_lag * 0.12),
            SubagentRole::General | SubagentRole::Custom(_) => {
                (1.4 + breath_lag * 0.35, 0.7 - breath_lag * 0.22)
            }
            SubagentRole::Explore => (0.0, 0.0),
        }
    };

    if !animate {
        let (prop_dx, prop_dy) = base_prop(0.0);
        return Pose3D {
            bob_y: 0.35,
            yaw: -0.18,
            pitch: 0.18,
            roll: 0.09,
            prop_dx,
            prop_dy,
            fx: 0.35,
            ..Pose3D::default()
        };
    }

    let envelope = |phase_u: f32| -> f32 {
        if phase_u < 0.42 {
            ease_in_out_cubic(phase_u / 0.42)
        } else if phase_u < 0.52 {
            1.0
        } else if phase_u < 0.90 {
            1.0 - ease_in_out_cubic((phase_u - 0.52) / 0.38)
        } else {
            0.0
        }
    };

    let u = time.rem_euclid(3.2) / 3.2;
    let breath = envelope(u);
    let breath_lag = envelope((time - 0.22).rem_euclid(3.2) / 3.2);

    let (sx, sy) = squash_stretch(lerp(-0.055, 0.055, breath));
    let bob_y = lerp(0.45, -0.4, breath);
    let (prop_dx, prop_dy) = base_prop(breath_lag);

    Pose3D {
        bob_y,
        lean_x: (breath - 0.5) * 0.15,
        sx,
        sy,
        yaw: -0.18 + (breath - 0.5) * 0.05,
        pitch: lerp(0.20, 0.11, breath),
        roll: lerp(0.11, 0.06, breath),
        gaze_x: 0.0,
        gaze_y: 0.2,
        eye_scale: 1.0,
        prop_dx,
        prop_dy,
        fx: u,
        aux: breath,
    }
}

// ---------------------------------------------------------------------------
// 3D Rounded-Box Rendering
// ---------------------------------------------------------------------------

/// Paint a filled 3D rounded rectangle lying in the local plane `z = z_plane`.
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

/// Paint a 3D rounded-corner box using layered back-to-front extrusion + directional
/// top/side shading + top-left front rim highlight for crisp matte-vinyl volume.
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
    // 1. Extrude back-to-front slices to build smooth 3D rounded side & top/bottom walls.
    const SLICES: usize = 7;
    for i in 0..SLICES {
        let t = i as f32 / (SLICES - 1) as f32; // 0 = back, 1 = just behind front
        let z = lerp(-hz, hz * 0.88, t);
        // Subtle bevel taper on the very back slice
        let inset = if i == 0 { 0.35 } else { 0.0 };
        // Deeper shadow on back/underside, warming smoothly toward the front edge.
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

    // 2. Lit top cap highlight when the box pitches forward (visible top face catching overhead light!).
    if cam.pitch > 0.01 {
        let top_vis = (cam.pitch * 3.5).clamp(0.0, 1.0);
        let top_col = adjust_lightness(base, if dark_theme { 0.12 } else { 0.10 }).opacity(0.65 * top_vis);
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

    // 3. Front face (slightly inset for a crisp beveled 3D edge).
    let front_col = adjust_lightness(base, if dark_theme { 0.04 } else { 0.03 });
    paint_rounded_plane(cam, hx - 0.15, hy - 0.15, radius, hz, front_col, window);

    // 4. Subtle top-left specular bevel sheen on the front face (gives tactile clay/vinyl luster).
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

/// Paint a 3D shaded sphere (base + bottom shadow crescent + top-left specular dot).
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
    // Outer shadow sphere
    let shadow = adjust_lightness(base, -0.12);
    window.paint_quad(
        fill(
            Bounds::new(point(pt.x - px(r), pt.y - px(r)), size(px(r * 2.0), px(r * 2.0))),
            shadow,
        )
        .corner_radii(px(r)),
    );
    // Main lit body shifted slightly up-left
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
    // Crisp top-left specular glint
    let r_spec = (r * 0.32).max(0.75);
    let spec = adjust_lightness(base, if dark_theme { 0.28 } else { 0.24 }).opacity(0.85);
    window.paint_quad(
        fill(
            Bounds::new(
                point(pt.x - px(r * 0.38 + r_spec), pt.y - px(r * 0.38 + r_spec)),
                size(px(r_spec * 2.0), px(r_spec * 2.0)),
            ),
            spec,
        )
        .corner_radii(px(r_spec)),
    );
}

/// Paint a cute 3D side ear/nub on the left (`side = -1`) or right (`side = +1`) of the box.
fn paint_3d_ear(
    cam: &Cam3D,
    hx: f32,
    side: f32,
    base: Hsla,
    window: &mut Window,
) {
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

/// Draw the complete 3D rounded-box creature inside `bounds`.
#[allow(clippy::too_many_arguments)]
fn paint_creature_3d(
    bounds: Bounds<Pixels>,
    role: SubagentRole,
    state: EntityState,
    elapsed: f32,
    phase: f32,
    animate: bool,
    visual_scale: f32,
    dy_px: f32,
    t: Tokens,
    window: &mut Window,
) {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    let s = (w.min(h) / 24.0) * visual_scale.clamp(0.1, 1.5);
    if s <= 0.05 {
        return;
    }
    let ox = f32::from(bounds.origin.x) + (w - 24.0 * s) * 0.5;
    let oy = f32::from(bounds.origin.y) + (h - 24.0 * s) * 0.5 + dy_px * crate::style::zoom();
    let time = elapsed + phase * 6.2831;
    let dark = is_dark(t.window);

    let pose = evaluate_pose(role, state, time, animate);

    // Rounded-box dimensions in local 3D space (centered at origin).
    let hx = 5.9;
    let hy = if matches!(role, SubagentRole::Worker) {
        5.1
    } else {
        5.4
    };
    let hz = 4.2;
    let radius = 2.4;

    let base_cy = if matches!(role, SubagentRole::Worker) {
        14.6
    } else {
        14.2
    };
    let cam = Cam3D {
        ox,
        oy,
        scale: s,
        cx: 12.0 + pose.lean_x,
        cy: base_cy + pose.bob_y,
        sx: pose.sx,
        sy: pose.sy,
        yaw: pose.yaw,
        pitch: pose.pitch,
        roll: pose.roll,
    };

    let body_color = body_palette(role, state, dark, t);
    let prop_tint = prop_palette(role, state, dark, t);
    // Deep ink eyes with crisp bright catchlights so faces pop against every colored box!
    let eye_color = if dark {
        c(0x1112_16FF)
    } else {
        c(0x1C1B_1AFF)
    };
    let catchlight_color = c(0xFFFF_FFFF).opacity(0.95);

    // 1. Soft floor shadow (anchored beneath the 3D box; shrinks & softens on lift).
    let lift_norm = (-pose.bob_y).max(0.0) / 2.2;
    let shadow_w = (12.0 - lift_norm * 3.4).max(5.5);
    let shadow_h = (2.1 - lift_norm * 0.55).max(1.0);
    let shadow_bounds = Bounds::new(
        point(
            px(ox + (12.0 + pose.lean_x * 0.55 - shadow_w * 0.5) * s),
            px(oy + 21.3 * s),
        ),
        size(px(shadow_w * s), px(shadow_h * s)),
    );
    let shadow_alpha = (if dark { 0.36 } else { 0.16 }) * (1.0 - lift_norm * 0.42);
    window.paint_quad(
        fill(shadow_bounds, c(0x0000_00FF).opacity(shadow_alpha))
            .corner_radii(px(shadow_h * 0.5 * s)),
    );

    // 2. Depth-sort the two side ears against the box center.
    let left_ear_z = cam.xform(Vec3::new(-(hx + 0.75), 0.1, 0.0)).z;
    let right_ear_z = cam.xform(Vec3::new(hx + 0.75, 0.1, 0.0)).z;
    if left_ear_z < 0.0 {
        paint_3d_ear(&cam, hx, -1.0, body_color, window);
    }
    if right_ear_z < 0.0 {
        paint_3d_ear(&cam, hx, 1.0, body_color, window);
    }

    // 3. Crown props that sit behind/atop the box before the front face.
    if matches!(role, SubagentRole::General) {
        paint_3d_antenna(&cam, hy, state, pose, animate, body_color, prop_tint, dark, window);
    } else if let SubagentRole::Custom(variant) = role {
        paint_3d_custom_crest(variant, &cam, hy, pose, animate, body_color, prop_tint, dark, window);
    }

    // 4. The 3D Rounded-Corner Box body!
    paint_3d_rounded_box(&cam, hx, hy, hz, radius, body_color, dark, window);

    // Near ear(s) in front of the side wall.
    if left_ear_z >= 0.0 {
        paint_3d_ear(&cam, hx, -1.0, body_color, window);
    }
    if right_ear_z >= 0.0 {
        paint_3d_ear(&cam, hx, 1.0, body_color, window);
    }

    // 5. Expressive face projected directly onto the 3D box's front plane (`z = hz + 0.12`).
    let face_z = hz + 0.12;
    let blink = compute_blink(time, animate);
    let show_eyes = !matches!(role, SubagentRole::Explore)
        || matches!(state, EntityState::Done | EntityState::Error | EntityState::Cancelled);

    if show_eyes {
        paint_3d_eyes(
            &cam,
            face_z,
            pose.gaze_x,
            pose.gaze_y,
            pose.eye_scale,
            blink,
            role,
            state,
            eye_color,
            catchlight_color,
            window,
        );
    }

    if !matches!(role, SubagentRole::Explore) {
        paint_3d_mouth(
            &cam,
            face_z,
            pose.gaze_x * 0.35,
            pose.aux,
            role,
            state,
            eye_color,
            window,
        );
    }

    // 6. 3D Role-specific foreground props & personality secondary FX.
    match role {
        SubagentRole::Worker => {
            paint_3d_hard_hat(&cam, hy, pose.prop_dx, pose.prop_dy, prop_tint, dark, window);
            if animate && state == EntityState::Running {
                paint_worker_sparks(ox, oy, s, pose.fx, prop_tint, window);
            }
        }
        SubagentRole::Explore => {
            paint_3d_binoculars(
                &cam,
                hz,
                pose.gaze_x,
                pose.prop_dx,
                pose.prop_dy,
                state,
                prop_tint,
                eye_color,
                dark,
                window,
            );
            if animate && state == EntityState::Running && pose.fx > 0.2 {
                let star_pt = cam.pt(Vec3::new(pose.prop_dx, -hy - 2.6 - pose.fx * 1.1, 0.0));
                paint_four_point_star(
                    star_pt,
                    (1.3 + pose.fx * 1.1) * s,
                    prop_tint.opacity(pose.fx * 0.92),
                    window,
                );
            }
        }
        SubagentRole::Plan => {
            let glasses_lift = if state == EntityState::Done {
                pose.prop_dy
            } else {
                -pose.fx * 0.35
            };
            paint_3d_glasses(&cam, hx, hz, glasses_lift, prop_tint, window);
            paint_3d_notebook_and_pen(&cam, hy, hz, state, pose, prop_tint, dark, window);
            if animate && state == EntityState::Running {
                paint_plan_thought_fx(&cam, hy, s, pose.prop_dy < -0.5, pose.fx, prop_tint, window);
            }
        }
        SubagentRole::General | SubagentRole::Custom(_) => {}
    }

    // 7. Sleeping floating `z`s along a curved S-arc when `Done`.
    if state == EntityState::Done {
        paint_sleep_zs(ox, oy, s, pose.fx, animate, c(t.text_muted), window);
    }

    // 8. Gentle attention pip when blocked/awaiting input.
    if state == EntityState::Blocked {
        let pulse = if animate {
            0.5 + 0.5 * (time * 3.8).sin().abs()
        } else {
            0.9
        };
        let pip = cam.pt(Vec3::new(hx + 0.8, -hy - 0.6, hz));
        let d = 3.6 * s;
        window.paint_quad(
            fill(
                Bounds::new(point(pip.x - px(d * 0.5), pip.y - px(d * 0.5)), size(px(d), px(d))),
                c(t.warning).opacity(pulse),
            )
            .corner_radii(px(d * 0.5)),
        );
    }
}

/// Natural irregular blink factor in `0.2..=1.0` (with occasional double-blink).
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

#[allow(clippy::too_many_arguments)]
fn paint_3d_eyes(
    cam: &Cam3D,
    face_z: f32,
    gaze_x: f32,
    gaze_y: f32,
    eye_scale: f32,
    blink: f32,
    role: SubagentRole,
    state: EntityState,
    eye_color: Hsla,
    catchlight_color: Hsla,
    window: &mut Window,
) {
    let eye_y = -0.35 + gaze_y;
    let xs = [-2.35 + gaze_x * 0.8, 2.35 + gaze_x * 0.8];

    match state {
        EntityState::Error => {
            let r = 1.05;
            let sw = (1.15 * cam.scale).max(1.0);
            for cx in [-2.35, 2.35] {
                let mut pb = PathBuilder::stroke(px(sw));
                pb.move_to(cam.pt(Vec3::new(cx - r, -0.35 - r, face_z)));
                pb.line_to(cam.pt(Vec3::new(cx + r, -0.35 + r, face_z)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, eye_color);
                }
                let mut pb2 = PathBuilder::stroke(px(sw));
                pb2.move_to(cam.pt(Vec3::new(cx + r, -0.35 - r, face_z)));
                pb2.line_to(cam.pt(Vec3::new(cx - r, -0.35 + r, face_z)));
                if let Ok(path) = pb2.build() {
                    window.paint_path(path, eye_color);
                }
            }
        }
        EntityState::Cancelled => {
            let sw = (1.15 * cam.scale).max(1.0);
            for cx in [-2.35, 2.35] {
                let mut pb = PathBuilder::stroke(px(sw));
                pb.move_to(cam.pt(Vec3::new(cx - 1.15, -0.35, face_z)));
                pb.line_to(cam.pt(Vec3::new(cx + 1.15, -0.35, face_z)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, eye_color);
                }
            }
        }
        EntityState::Done => {
            // Peaceful closed sleeping eyelids (◡  ◡) mapped onto the 3D front face.
            let sw = (1.15 * cam.scale).max(1.0);
            for cx in [-2.35, 2.35] {
                let mut pb = PathBuilder::stroke(px(sw));
                pb.move_to(cam.pt(Vec3::new(cx - 1.15, -0.5, face_z)));
                pb.line_to(cam.pt(Vec3::new(cx - 0.45, 0.35, face_z)));
                pb.line_to(cam.pt(Vec3::new(cx + 0.45, 0.35, face_z)));
                pb.line_to(cam.pt(Vec3::new(cx + 1.15, -0.5, face_z)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, eye_color);
                }
            }
        }
        EntityState::Running | EntityState::Blocked => {
            let base_h = match (role, state) {
                (_, EntityState::Blocked) => 2.6,
                (SubagentRole::Worker, _) => 2.15,
                (SubagentRole::Plan, _) => 2.15,
                _ => 2.5,
            } * eye_scale;
            let eh = (base_h * blink).max(0.65);
            let ew = if matches!(role, SubagentRole::Plan) {
                1.85
            } else {
                2.05
            };
            for cx in xs {
                let (pt, persp, _) = cam.project(Vec3::new(cx, eye_y, face_z));
                // Foreshorten horizontal eye width slightly as the box yaws!
                let yaw_foreshorten = cam.yaw.cos().max(0.65);
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
                    let cd = (0.78 * persp * cam.scale).max(0.85);
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                point(pt.x - px(pw * 0.24 + cd * 0.4), pt.y - px(ph * 0.24 + cd * 0.4)),
                                size(px(cd), px(cd)),
                            ),
                            catchlight_color,
                        )
                        .corner_radii(px(cd * 0.5)),
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_3d_mouth(
    cam: &Cam3D,
    face_z: f32,
    dx: f32,
    aux: f32,
    role: SubagentRole,
    state: EntityState,
    eye_color: Hsla,
    window: &mut Window,
) {
    let my = 2.45;
    let sw = (0.95 * cam.scale).max(0.85);
    match state {
        EntityState::Done => {
            let (pt, persp, _) = cam.project(Vec3::new(dx, my, face_z));
            let d = (0.95 + (1.0 - aux) * 0.55) * persp * cam.scale;
            window.paint_quad(
                fill(
                    Bounds::new(
                        point(pt.x - px(d * 0.5), pt.y - px(d * 0.45)),
                        size(px(d), px(d * 0.9)),
                    ),
                    eye_color.opacity(0.8),
                )
                .corner_radii(px(d * 0.5)),
            );
        }
        EntityState::Blocked => {
            let (pt, persp, _) = cam.project(Vec3::new(dx, my, face_z));
            let d = 1.4 * persp * cam.scale;
            window.paint_quad(
                fill(
                    Bounds::new(point(pt.x - px(d * 0.5), pt.y - px(d * 0.5)), size(px(d), px(d))),
                    eye_color.opacity(0.85),
                )
                .corner_radii(px(d * 0.5)),
            );
        }
        EntityState::Error => {
            let mut pb = PathBuilder::stroke(px(sw));
            pb.move_to(cam.pt(Vec3::new(dx - 1.05, my + 0.35, face_z)));
            pb.line_to(cam.pt(Vec3::new(dx, my - 0.3, face_z)));
            pb.line_to(cam.pt(Vec3::new(dx + 1.05, my + 0.35, face_z)));
            if let Ok(path) = pb.build() {
                window.paint_path(path, eye_color.opacity(0.85));
            }
        }
        EntityState::Running => match role {
            SubagentRole::Worker => {
                let mut pb = PathBuilder::stroke(px(sw));
                pb.move_to(cam.pt(Vec3::new(dx - 0.95, my, face_z)));
                pb.line_to(cam.pt(Vec3::new(dx + 0.95, my - 0.15, face_z)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, eye_color.opacity(0.8));
                }
            }
            SubagentRole::Plan => {
                let mut pb = PathBuilder::stroke(px(sw));
                pb.move_to(cam.pt(Vec3::new(dx - 0.55, my, face_z)));
                pb.line_to(cam.pt(Vec3::new(dx + 0.75, my, face_z)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, eye_color.opacity(0.75));
                }
            }
            _ => {
                let mut pb = PathBuilder::stroke(px(sw));
                pb.move_to(cam.pt(Vec3::new(dx - 1.05, my - 0.2, face_z)));
                pb.line_to(cam.pt(Vec3::new(dx, my + 0.45, face_z)));
                pb.line_to(cam.pt(Vec3::new(dx + 1.05, my - 0.2, face_z)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, eye_color.opacity(0.8));
                }
            }
        },
        EntityState::Cancelled => {}
    }
}

/// Realistic 3D Construction Hard Hat for `worker`:
/// - Smooth hemispherical 3D dome shell (not stacked flat rings) with directional shading
///   and a curved specular highlight.
/// - Forward-projecting front visor / peak (with dark underside cast shadow) + slim side lip.
/// - Iconic raised 3D center structural rib (lit top + shaded side) + subtle side ribs.
fn paint_3d_hard_hat(
    cam: &Cam3D,
    hy: f32,
    lag_x: f32,
    lag_y: f32,
    tint: Hsla,
    dark: bool,
    window: &mut Window,
) {
    let base_y = -hy + 0.55 + lag_y;
    let dome_h = 5.1;
    let dome_rx = 6.35;
    let dome_rz = 4.9;

    let shell_shadow = adjust_lightness(tint, -0.17);
    let shell_base = tint;
    let shell_lit = adjust_lightness(tint, if dark { 0.09 } else { 0.07 });
    let shell_spec = adjust_lightness(tint, if dark { 0.24 } else { 0.20 }).opacity(0.72);
    let visor_under = adjust_lightness(tint, -0.26);
    let visor_top = adjust_lightness(tint, -0.04);
    let rib_side = adjust_lightness(tint, -0.11);
    let rib_top = adjust_lightness(tint, if dark { 0.16 } else { 0.13 });

    // Helper: draw a 3D dome arch lying at depth `z_off`, scaled by `r_scale`.
    let paint_dome_shell = |z_off: f32, r_scale: f32, col: Hsla, window: &mut Window| {
        let mut pb = PathBuilder::fill();
        const SEG: usize = 16;
        let rx = dome_rx * r_scale;
        let ry = dome_h * r_scale;
        for i in 0..=SEG {
            // Arch from right (+rx) over the top (-ry) to left (-rx)
            let a = (i as f32 / SEG as f32) * std::f32::consts::PI;
            let sin_a = a.sin().max(0.0);
            let x = lag_x + rx * a.cos();
            let y = base_y - ry * sin_a.powf(0.88);
            let pt = cam.pt(Vec3::new(x, y, z_off));
            if i == 0 {
                pb.move_to(pt);
            } else {
                pb.line_to(pt);
            }
        }
        // Slightly curved lower rim following the brow
        pb.line_to(cam.pt(Vec3::new(lag_x, base_y + 0.35, z_off + 0.6)));
        pb.close();
        if let Ok(path) = pb.build() {
            window.paint_path(path, col);
        }
    };

    // 1. Dark cast shadow band right under the hat brim on the creature's forehead.
    let mut brow_shadow = PathBuilder::fill();
    brow_shadow.move_to(cam.pt(Vec3::new(lag_x - 6.0, base_y + 0.15, dome_rz - 0.4)));
    brow_shadow.line_to(cam.pt(Vec3::new(lag_x + 6.0, base_y + 0.15, dome_rz - 0.4)));
    brow_shadow.line_to(cam.pt(Vec3::new(lag_x + 5.7, base_y + 1.15, dome_rz - 0.4)));
    brow_shadow.line_to(cam.pt(Vec3::new(lag_x - 5.7, base_y + 1.15, dome_rz - 0.4)));
    brow_shadow.close();
    if let Ok(path) = brow_shadow.build() {
        window.paint_path(path, c(0x0000_00FF).opacity(if dark { 0.38 } else { 0.22 }));
    }

    // 2. Smooth 3D hemispherical dome extruded back-to-front for genuine volume.
    paint_dome_shell(-dome_rz * 0.62, 0.96, shell_shadow, window);
    paint_dome_shell(-dome_rz * 0.15, 1.00, adjust_lightness(shell_base, -0.07), window);
    paint_dome_shell(dome_rz * 0.32, 0.99, shell_base, window);
    paint_dome_shell(dome_rz * 0.66, 0.94, shell_lit, window);

    // 3. Curved specular gloss crescent on the upper-left dome (plastic hard-hat shine).
    let mut spec = PathBuilder::fill();
    let spec_pts = [
        Vec3::new(lag_x - 4.4, base_y - 1.7, dome_rz * 0.72),
        Vec3::new(lag_x - 2.6, base_y - 3.6, dome_rz * 0.62),
        Vec3::new(lag_x - 1.5, base_y - 3.2, dome_rz * 0.72),
        Vec3::new(lag_x - 3.2, base_y - 1.4, dome_rz * 0.80),
    ];
    spec.move_to(cam.pt(spec_pts[0]));
    for &v in &spec_pts[1..] {
        spec.line_to(cam.pt(v));
    }
    spec.close();
    if let Ok(path) = spec.build() {
        window.paint_path(path, shell_spec);
    }

    // 4. Subtle side structural ribs flanking the center ridge.
    let side_rib_sw = (0.75 * cam.scale).max(0.7);
    for side in [-1.0_f32, 1.0_f32] {
        let mut srib = PathBuilder::stroke(px(side_rib_sw));
        srib.move_to(cam.pt(Vec3::new(lag_x + side * 3.1, base_y - 0.5, dome_rz * 0.78)));
        srib.line_to(cam.pt(Vec3::new(lag_x + side * 2.4, base_y - 3.3, dome_rz * 0.45)));
        srib.line_to(cam.pt(Vec3::new(lag_x + side * 1.9, base_y - 4.1, -0.6)));
        if let Ok(path) = srib.build() {
            window.paint_path(path, rib_side.opacity(0.65));
        }
    }

    // 5. Iconic raised 3D center structural rib (crown fin) with shaded side + lit top!
    let mut rib_r = PathBuilder::fill();
    let rib_r_pts = [
        Vec3::new(lag_x + 0.0, base_y - 0.5, dome_rz + 0.35),
        Vec3::new(lag_x + 1.25, base_y - 0.4, dome_rz + 0.05),
        Vec3::new(lag_x + 1.15, base_y - 4.35, dome_rz * 0.35),
        Vec3::new(lag_x + 0.95, base_y - 5.45, -0.2),
        Vec3::new(lag_x + 0.0, base_y - 5.65, -0.2),
        Vec3::new(lag_x + 0.0, base_y - 4.55, dome_rz * 0.45),
    ];
    rib_r.move_to(cam.pt(rib_r_pts[0]));
    for &v in &rib_r_pts[1..] {
        rib_r.line_to(cam.pt(v));
    }
    rib_r.close();
    if let Ok(path) = rib_r.build() {
        window.paint_path(path, rib_side);
    }

    let mut rib_l = PathBuilder::fill();
    let rib_l_pts = [
        Vec3::new(lag_x - 1.25, base_y - 0.4, dome_rz + 0.05),
        Vec3::new(lag_x + 0.25, base_y - 0.45, dome_rz + 0.38),
        Vec3::new(lag_x + 0.25, base_y - 4.55, dome_rz * 0.45),
        Vec3::new(lag_x + 0.2, base_y - 5.65, -0.2),
        Vec3::new(lag_x - 1.0, base_y - 5.45, -0.2),
        Vec3::new(lag_x - 1.15, base_y - 4.35, dome_rz * 0.35),
    ];
    rib_l.move_to(cam.pt(rib_l_pts[0]));
    for &v in &rib_l_pts[1..] {
        rib_l.line_to(cam.pt(v));
    }
    rib_l.close();
    if let Ok(path) = rib_l.build() {
        window.paint_path(path, rib_top);
    }

    // 6. Forward-projecting construction visor / peak + slim wraparound side brim!
    // Underside shadow lip (thicker in front where the peak overhangs)
    let paint_visor = |y_off: f32, z_peak: f32, side_w: f32, col: Hsla, window: &mut Window| {
        let mut pb = PathBuilder::fill();
        const SEG: usize = 14;
        for i in 0..=SEG {
            // Front arc from left side (-side_w) curving forward to peak (+z_peak) to right (+side_w)
            let a = (i as f32 / SEG as f32) * std::f32::consts::PI;
            let sin_a = a.sin().max(0.0);
            let x = lag_x - side_w * a.cos();
            let z = dome_rz * 0.15 + z_peak * sin_a.powf(0.75);
            let y = base_y + y_off + 0.35 * sin_a; // slight downward bill curve at front peak
            let pt = cam.pt(Vec3::new(x, y, z));
            if i == 0 {
                pb.move_to(pt);
            } else {
                pb.line_to(pt);
            }
        }
        // Back inner edge hugging the brow
        for i in (0..=6).rev() {
            let a = (i as f32 / 6.0) * std::f32::consts::PI;
            let x = lag_x - (dome_rx * 0.96) * a.cos();
            let z = dome_rz * 0.1 + (dome_rz * 0.72) * a.sin();
            pb.line_to(cam.pt(Vec3::new(x, base_y + y_off - 0.15, z)));
        }
        pb.close();
        if let Ok(path) = pb.build() {
            window.paint_path(path, col);
        }
    };

    paint_visor(0.45, dome_rz + 2.45, 7.25, visor_under, window);
    paint_visor(0.0, dome_rz + 2.45, 7.2, visor_top, window);

    // Subtle crisp front visor edge highlight
    let mut edge = PathBuilder::stroke(px((0.75 * cam.scale).max(0.7)));
    for i in 0..=8 {
        let a = (i as f32 / 8.0) * std::f32::consts::PI;
        let sin_a = a.sin().max(0.0);
        let x = lag_x - 6.6 * a.cos();
        let z = dome_rz * 0.15 + (dome_rz + 2.35) * sin_a.powf(0.75);
        let y = base_y + 0.32 * sin_a;
        let pt = cam.pt(Vec3::new(x, y, z));
        if i == 0 {
            edge.move_to(pt);
        } else {
            edge.line_to(pt);
        }
    }
    if let Ok(path) = edge.build() {
        window.paint_path(path, shell_spec);
    }
}

/// Realistic 3D Tapered Binoculars for `explore`:
/// - Matte gunmetal tapered twin barrels flaring from eyepiece to objective bell.
/// - Rubberized grip band + central hinge bridge + brass/gold knurled focus wheel.
/// - Deep optical navy-glass lenses with teal anti-reflective coating & crisp glints.
/// - Cleanly tucked down on the chest when sleeping so closed eyes stay 100% visible.
#[allow(clippy::too_many_arguments)]
fn paint_3d_binoculars(
    cam: &Cam3D,
    hz: f32,
    gaze_x: f32,
    lead_x: f32,
    lift_y: f32,
    state: EntityState,
    accent: Hsla,
    _eye_color: Hsla,
    dark: bool,
    window: &mut Window,
) {
    let lowered = matches!(
        state,
        EntityState::Done | EntityState::Error | EntityState::Cancelled
    );
    // When sleeping/done, rest cleanly lower so the sleeping eyelids (◡ ◡) stay fully visible!
    let y = if state == EntityState::Done {
        3.15
    } else if lowered {
        3.25
    } else {
        -0.3 + lift_y
    };
    let scale_down = if lowered { 0.84 } else { 1.0 };

    let lx = (-2.65 * scale_down) + lead_x;
    let rx = (2.65 * scale_down) + lead_x;
    let z_back = hz + 0.15;
    let z_front = hz + 3.45 * scale_down;

    // Matte gunmetal housing (realistic optical body) + brass/gold accent rings & focus wheel!
    let body_dark = c(if dark { 0x2328_30FF } else { 0x2B31_3AFF });
    let body_mid = c(if dark { 0x3A42_4EFF } else { 0x434C_59FF });
    let body_hi = c(if dark { 0x5661_70FF } else { 0x626E_7EFF });
    let rubber_grip = c(if dark { 0x1D21_28FF } else { 0x262B_33FF });
    let glass_deep = c(0x0E18_26FF);
    let glass_coat = c(0x3DB8_C4FF).opacity(0.32);
    let glint_col = c(0xFFFF_FFFF).opacity(0.92);

    // Paint one tapered 3D binocular barrel (eyepiece -> rubber grip -> objective bell -> lens).
    let paint_barrel = |cx: f32, window: &mut Window| {
        const SLICES: usize = 9;
        for i in 0..SLICES {
            let k = i as f32 / (SLICES - 1) as f32;
            let z = lerp(z_back, z_front, k);
            // Realistic binocular taper: narrower eyepiece flaring toward objective lens bell!
            let r_base = lerp(1.65, 2.55, k.powf(0.85)) * scale_down;
            let (pt, persp, _) = cam.project(Vec3::new(cx, y, z));
            let r = r_base * persp * cam.scale;

            let col = if i == SLICES - 1 {
                // Crisp outer beveled metal objective bezel
                body_hi
            } else if (3..=5).contains(&i) {
                // Darker rubberized grip collar around mid-barrel
                rubber_grip
            } else if i == SLICES - 2 {
                // Thin brass/gold optical accent ring just behind the objective rim
                accent
            } else {
                adjust_lightness(body_dark, k * 0.14)
            };

            window.paint_quad(
                fill(
                    Bounds::new(point(pt.x - px(r), pt.y - px(r)), size(px(r * 2.0), px(r * 2.0))),
                    col,
                )
                .corner_radii(px(r)),
            );

            // Top specular cylinder highlight along the upper edge of the barrel
            if i > 0 && i < SLICES - 1 && !(3..=5).contains(&i) {
                let hr = (r * 0.34).max(0.6);
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(pt.x - px(r * 0.35 + hr * 0.5), pt.y - px(r * 0.72)),
                            size(px(hr * 1.4), px(hr * 0.75)),
                        ),
                        body_hi.opacity(0.55),
                    )
                    .corner_radii(px(hr * 0.4)),
                );
            }
        }

        // Inner dark step bezel + deep optical glass lens
        let (pt, persp, _) = cam.project(Vec3::new(cx, y, z_front + 0.06));
        let outer_lr = 2.05 * scale_down * persp * cam.scale;
        let inner_lr = 1.72 * scale_down * persp * cam.scale;
        window.paint_quad(
            fill(
                Bounds::new(
                    point(pt.x - px(outer_lr), pt.y - px(outer_lr)),
                    size(px(outer_lr * 2.0), px(outer_lr * 2.0)),
                ),
                body_dark,
            )
            .corner_radii(px(outer_lr)),
        );
        window.paint_quad(
            fill(
                Bounds::new(
                    point(pt.x - px(inner_lr), pt.y - px(inner_lr)),
                    size(px(inner_lr * 2.0), px(inner_lr * 2.0)),
                ),
                glass_deep,
            )
            .corner_radii(px(inner_lr)),
        );

        // Teal anti-reflective optical lens sheen crescent (lower-right inside lens)
        let cr = inner_lr * 0.68;
        window.paint_quad(
            fill(
                Bounds::new(
                    point(pt.x - px(cr * 0.35), pt.y - px(cr * 0.25)),
                    size(px(cr * 1.3), px(cr * 1.15)),
                ),
                glass_coat,
            )
            .corner_radii(px(cr * 0.6)),
        );

        // Crisp angled optical reflection glint (shifts as explorer scans!)
        let gx_shift = if lowered { 0.0 } else { gaze_x * 0.35 };
        let gr = (0.52 * scale_down * persp * cam.scale).max(0.75);
        let gx = (-0.52 * scale_down + gx_shift) * persp * cam.scale;
        let gy = -0.52 * scale_down * persp * cam.scale;
        window.paint_quad(
            fill(
                Bounds::new(
                    point(pt.x + px(gx - gr), pt.y + px(gy - gr)),
                    size(px(gr * 2.0), px(gr * 2.0)),
                ),
                glint_col,
            )
            .corner_radii(px(gr)),
        );
        // Tiny secondary opposing lens sparkle
        let gr2 = (gr * 0.45).max(0.5);
        window.paint_quad(
            fill(
                Bounds::new(
                    point(pt.x - px(gx * 0.7 + gr2), pt.y - px(gy * 0.7 + gr2)),
                    size(px(gr2 * 2.0), px(gr2 * 2.0)),
                ),
                glint_col.opacity(0.55),
            )
            .corner_radii(px(gr2)),
        );
    };

    // Depth-sort far barrel → center hinge + brass focus wheel → near barrel!
    let lz = cam.xform(Vec3::new(lx, y, z_front)).z;
    let rz = cam.xform(Vec3::new(rx, y, z_front)).z;
    let (far_x, near_x) = if lz < rz { (lx, rx) } else { (rx, lx) };

    paint_barrel(far_x, window);

    // Center hinge bridge (front + back struts)
    let z_mid = (z_back + z_front) * 0.52;
    let mut bridge = PathBuilder::stroke(px((1.55 * scale_down * cam.scale).max(1.1)));
    bridge.move_to(cam.pt(Vec3::new(lx + 0.8 * scale_down, y, z_mid)));
    bridge.line_to(cam.pt(Vec3::new(rx - 0.8 * scale_down, y, z_mid)));
    if let Ok(path) = bridge.build() {
        window.paint_path(path, body_mid);
    }

    // Center brass/gold knurled focus wheel sitting atop the bridge!
    let (fpt, fpersp, _) = cam.project(Vec3::new(lead_x, y - 0.85 * scale_down, z_mid + 0.3));
    let fw = 1.35 * scale_down * fpersp * cam.scale;
    let fh = 1.85 * scale_down * fpersp * cam.scale;
    window.paint_quad(
        fill(
            Bounds::new(point(fpt.x - px(fw * 0.5), fpt.y - px(fh * 0.5)), size(px(fw), px(fh))),
            adjust_lightness(accent, -0.06),
        )
        .corner_radii(px(fw * 0.35)),
    );
    window.paint_quad(
        fill(
            Bounds::new(
                point(fpt.x - px(fw * 0.32), fpt.y - px(fh * 0.38)),
                size(px(fw * 0.64), px(fh * 0.32)),
            ),
            adjust_lightness(accent, 0.16),
        )
        .corner_radii(px(fw * 0.2)),
    );

    paint_barrel(near_x, window);
}

/// 3D Round Architect Glasses for `plan`: true 3D circles projecting to perspective ellipses!
fn paint_3d_glasses(
    cam: &Cam3D,
    hx: f32,
    hz: f32,
    lift_y: f32,
    tint: Hsla,
    window: &mut Window,
) {
    let y = -0.35 + lift_y;
    let z = hz + 0.85;
    let r = 2.45;
    let sw = (1.25 * cam.scale).max(1.0);
    let lx = -2.5;
    let rx = 2.5;

    for cx in [lx, rx] {
        // Translucent 3D glass fill
        let mut fill_pb = PathBuilder::fill();
        let mut rim_pb = PathBuilder::stroke(px(sw));
        const SEG: usize = 16;
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
            window.paint_path(path, tint.opacity(0.16));
        }
        if let Ok(path) = rim_pb.build() {
            window.paint_path(path, tint);
        }
    }

    // Nose bridge
    let mut bridge = PathBuilder::stroke(px(sw * 0.95));
    bridge.move_to(cam.pt(Vec3::new(lx + r - 0.1, y - 0.1, z)));
    bridge.line_to(cam.pt(Vec3::new(rx - r + 0.1, y - 0.1, z)));
    if let Ok(path) = bridge.build() {
        window.paint_path(path, tint);
    }

    // 3D Temple arms going back toward the sides of the rounded box
    for (edge_x, side_x) in [(lx - r, -hx - 0.1), (rx + r, hx + 0.1)] {
        let mut arm = PathBuilder::stroke(px(sw * 0.8));
        arm.move_to(cam.pt(Vec3::new(edge_x, y - 0.25, z)));
        arm.line_to(cam.pt(Vec3::new(side_x, y - 0.55, 0.4)));
        if let Ok(path) = arm.build() {
            window.paint_path(path, tint.opacity(0.88));
        }
    }
}

/// 3D Springy Antenna for `general`: 2-segment stem + 3D shaded sphere orb + concentric signal ping.
#[allow(clippy::too_many_arguments)]
fn paint_3d_antenna(
    cam: &Cam3D,
    hy: f32,
    state: EntityState,
    pose: Pose3D,
    animate: bool,
    stem_color: Hsla,
    tip_color: Hsla,
    dark: bool,
    window: &mut Window,
) {
    let base = Vec3::new(0.0, -hy + 0.2, 0.0);
    let mid = Vec3::new(pose.prop_dx * 0.45, -hy - 2.1 + pose.prop_dy * 0.45, 0.1);
    let tip = Vec3::new(pose.prop_dx, -hy - 4.2 + pose.prop_dy, 0.2);

    let mut stem = PathBuilder::stroke(px((1.35 * cam.scale).max(1.0)));
    stem.move_to(cam.pt(base));
    stem.line_to(cam.pt(mid));
    stem.line_to(cam.pt(tip));
    if let Ok(path) = stem.build() {
        window.paint_path(path, adjust_lightness(stem_color, -0.06));
    }

    let pulsing = animate && state == EntityState::Running;
    if pulsing {
        let (pt, persp, _) = cam.project(tip);
        let ring_r = (1.6 + ease_out_cubic(pose.fx) * 2.4) * persp * cam.scale;
        let ring_alpha = (1.0 - pose.fx).powi(2) * 0.48;
        window.paint_quad(
            fill(
                Bounds::new(
                    point(pt.x - px(ring_r), pt.y - px(ring_r)),
                    size(px(ring_r * 2.0), px(ring_r * 2.0)),
                ),
                tip_color.opacity(ring_alpha),
            )
            .corner_radii(px(ring_r)),
        );
    }

    let orb_r = 1.65 + if pulsing { arc(pose.fx) * 0.22 } else { 0.0 };
    paint_3d_sphere(cam, tip, orb_r, tip_color, dark, window);
}

/// 3D Twin-orb antennae or floating crystal crest for custom agents.
#[allow(clippy::too_many_arguments)]
fn paint_3d_custom_crest(
    variant: u8,
    cam: &Cam3D,
    hy: f32,
    pose: Pose3D,
    animate: bool,
    stem_color: Hsla,
    tint: Hsla,
    dark: bool,
    window: &mut Window,
) {
    if variant % 2 == 0 {
        let sway = if animate { pose.prop_dx * 0.5 } else { 0.0 };
        for (bx, tx) in [(-2.0, -3.1 + sway), (2.0, 3.1 + sway)] {
            let base = Vec3::new(bx, -hy + 0.2, 0.0);
            let tip = Vec3::new(tx, -hy - 3.4 + pose.prop_dy * 0.4, 0.15);
            let mut pb = PathBuilder::stroke(px((1.2 * cam.scale).max(1.0)));
            pb.move_to(cam.pt(base));
            pb.line_to(cam.pt(tip));
            if let Ok(path) = pb.build() {
                window.paint_path(path, adjust_lightness(stem_color, -0.06));
            }
            paint_3d_sphere(cam, tip, 1.15, tint, dark, window);
        }
    } else {
        let cy = -hy - 3.1 + if animate { pose.prop_dy * 0.6 } else { 0.0 };
        paint_four_point_star(cam.pt(Vec3::new(0.0, cy, 0.0)), 2.2 * cam.scale, tint, window);
    }
}

/// Parabolic golden sparks bursting outward on each worker impact beat.
fn paint_worker_sparks(ox: f32, oy: f32, s: f32, fx: f32, tint: Hsla, window: &mut Window) {
    if fx <= 0.01 || fx >= 0.99 {
        return;
    }
    let alpha = (1.0 - fx).powf(1.4);
    let sparks = [
        (19.4, 11.4, 3.2, -4.2, 1.65),
        (19.7, 12.5, 2.5, -2.0, 1.25),
    ];
    for (sx0, sy0, vx, vy, base_d) in sparks {
        let x = sx0 + vx * ease_out_cubic(fx);
        let y = sy0 + vy * fx + 3.2 * fx * fx;
        let d = base_d * (1.0 - fx * 0.45) * s;
        let pt = point(px(ox + x * s - d * 0.5), px(oy + y * s - d * 0.5));
        window.paint_quad(
            fill(Bounds::new(pt, size(px(d), px(d))), tint.opacity(alpha)).corner_radii(px(d * 0.5)),
        );
    }
}

/// 3D Notebook & Pencil held by `plan`:
/// - Tilted 3D bound notebook (dark cover edge + warm cream paper + spine) held at
///   the lower-front so the creature can look down and write on it.
/// - Progressive ink lines drawn across the page as the pencil scribbles left-to-right.
/// - 3D pencil lifts to the cheek while pondering, arcs down to write Line 1 & Line 2,
///   and rests peacefully across the notebook when sleeping (`Done`).
#[allow(clippy::too_many_arguments)]
fn paint_3d_notebook_and_pen(
    cam: &Cam3D,
    hy: f32,
    hz: f32,
    state: EntityState,
    pose: Pose3D,
    accent: Hsla,
    dark: bool,
    window: &mut Window,
) {
    // Notebook geometry in creature-local 3D space (tilted slightly upward like a held pad).
    let nb_y_top = hy - 2.15;
    let nb_y_bot = hy + 1.55;
    let nb_z_top = hz + 1.25;
    let nb_z_bot = hz + 2.65;
    let nb_x_l = -4.2;
    let nb_x_r = 3.2;

    let cover_col = c(if dark { 0x2632_4DFF } else { 0x2230_52FF });
    let spine_col = adjust_lightness(cover_col, -0.10);
    let paper_col = c(if dark { 0xF3ED_DDFF } else { 0xFAF6_EAFF });
    let paper_edge = c(if dark { 0xD2C9_B4FF } else { 0xDCD3_BEFF });
    let ink_col = c(0x3B53_8EFF).opacity(0.85);

    // 1. Dark hard-cover backing (slightly wider & deeper for real 3D notebook thickness)
    let mut cover = PathBuilder::fill();
    cover.move_to(cam.pt(Vec3::new(nb_x_l - 0.35, nb_y_top + 0.15, nb_z_top - 0.35)));
    cover.line_to(cam.pt(Vec3::new(nb_x_r + 0.35, nb_y_top + 0.15, nb_z_top - 0.35)));
    cover.line_to(cam.pt(Vec3::new(nb_x_r + 0.35, nb_y_bot + 0.45, nb_z_bot + 0.15)));
    cover.line_to(cam.pt(Vec3::new(nb_x_l - 0.35, nb_y_bot + 0.45, nb_z_bot + 0.15)));
    cover.close();
    if let Ok(path) = cover.build() {
        window.paint_path(path, cover_col);
    }

    // 2. Page block thickness edge at the bottom
    let mut edge = PathBuilder::fill();
    edge.move_to(cam.pt(Vec3::new(nb_x_l, nb_y_bot, nb_z_bot)));
    edge.line_to(cam.pt(Vec3::new(nb_x_r, nb_y_bot, nb_z_bot)));
    edge.line_to(cam.pt(Vec3::new(nb_x_r, nb_y_bot + 0.32, nb_z_bot + 0.08)));
    edge.line_to(cam.pt(Vec3::new(nb_x_l, nb_y_bot + 0.32, nb_z_bot + 0.08)));
    edge.close();
    if let Ok(path) = edge.build() {
        window.paint_path(path, paper_edge);
    }

    // 3. Warm cream paper page
    let mut page = PathBuilder::fill();
    page.move_to(cam.pt(Vec3::new(nb_x_l, nb_y_top, nb_z_top)));
    page.line_to(cam.pt(Vec3::new(nb_x_r, nb_y_top, nb_z_top)));
    page.line_to(cam.pt(Vec3::new(nb_x_r, nb_y_bot, nb_z_bot)));
    page.line_to(cam.pt(Vec3::new(nb_x_l, nb_y_bot, nb_z_bot)));
    page.close();
    if let Ok(path) = page.build() {
        window.paint_path(path, paper_col);
    }

    // 4. Left binding spine strip
    let mut spine = PathBuilder::fill();
    spine.move_to(cam.pt(Vec3::new(nb_x_l - 0.35, nb_y_top, nb_z_top - 0.05)));
    spine.line_to(cam.pt(Vec3::new(nb_x_l + 0.55, nb_y_top, nb_z_top + 0.05)));
    spine.line_to(cam.pt(Vec3::new(nb_x_l + 0.55, nb_y_bot + 0.35, nb_z_bot + 0.08)));
    spine.line_to(cam.pt(Vec3::new(nb_x_l - 0.35, nb_y_bot + 0.35, nb_z_bot + 0.05)));
    spine.close();
    if let Ok(path) = spine.build() {
        window.paint_path(path, spine_col);
    }

    // Helper: point on the tilted notebook surface `(u in 0..1, v in 0..1)`.
    let page_pt = |u: f32, v: f32| -> Vec3 {
        Vec3::new(
            lerp(nb_x_l + 1.15, nb_x_r - 0.75, u),
            lerp(nb_y_top + 0.85, nb_y_bot - 0.75, v),
            lerp(nb_z_top + 0.15, nb_z_bot + 0.15, v),
        )
    };

    // 5. Progressive ink lines drawn onto the page!
    let ink_sw = (0.85 * cam.scale).max(0.75);
    let write_progress = if state == EntityState::Done {
        1.0
    } else if state == EntityState::Running {
        pose.aux
    } else {
        0.5
    };
    let line1 = (write_progress * 2.0).clamp(0.0, 1.0);
    let line2 = ((write_progress - 0.5) * 2.0).clamp(0.0, 1.0);

    if line1 > 0.02 {
        let mut l1 = PathBuilder::stroke(px(ink_sw));
        l1.move_to(cam.pt(page_pt(0.0, 0.22)));
        l1.line_to(cam.pt(page_pt(line1, 0.22)));
        if let Ok(path) = l1.build() {
            window.paint_path(path, ink_col);
        }
    }
    if line2 > 0.02 {
        let mut l2 = PathBuilder::stroke(px(ink_sw));
        l2.move_to(cam.pt(page_pt(0.0, 0.74)));
        l2.line_to(cam.pt(page_pt(line2 * 0.82, 0.74)));
        if let Ok(path) = l2.build() {
            window.paint_path(path, ink_col);
        }
    }

    // 6. 3D Pencil / Stylus:
    //    - Thinking (`prop_dy < 0`): lifted near the right cheek with subtle tap.
    //    - Writing (`prop_dy >= 0.5`): tip touches Line 1 or Line 2 and scribbles!
    //    - Sleeping (`Done`): resting neatly diagonally across the notebook.
    let (tip, tail) = if state == EntityState::Done {
        let t0 = page_pt(0.25, 0.55);
        let t1 = Vec3::new(t0.x + 3.8, t0.y - 1.35, t0.z + 0.45);
        (t0, t1)
    } else if state == EntityState::Running && pose.prop_dy >= 0.35 {
        let line_v = if pose.prop_dy >= 1.5 { 0.74 } else { 0.22 };
        let lift = if pose.prop_dy < 0.95 {
            (1.0 - pose.prop_dy) * 1.1
        } else {
            0.0
        };
        let on_page = page_pt(pose.prop_dx.clamp(0.0, 1.0), line_v);
        let t0 = Vec3::new(on_page.x, on_page.y - lift, on_page.z + 0.15 + lift * 0.4);
        let t1 = Vec3::new(t0.x + 2.8, t0.y - 3.6, t0.z + 0.85);
        (t0, t1)
    } else {
        // Raised thinking pose near the creature's right cheek
        let tap = pose.prop_dx;
        let t0 = Vec3::new(3.5 + tap * 0.4, nb_y_top - 1.5 + pose.prop_dy * 0.5, nb_z_top + 1.0);
        let t1 = Vec3::new(t0.x + 2.6, t0.y - 3.5, t0.z + 0.7);
        (t0, t1)
    };

    let pen_sw = (1.55 * cam.scale).max(1.2);
    let wood_col = accent;
    let tip_col = c(0x2326_2EFF);
    let eraser_col = c(0xE88D_8DFF);

    // Pencil shaft
    let mut shaft = PathBuilder::stroke(px(pen_sw));
    shaft.move_to(cam.pt(tip));
    shaft.line_to(cam.pt(tail));
    if let Ok(path) = shaft.build() {
        window.paint_path(path, wood_col);
    }

    // Graphite tip point
    let tip_mid = Vec3::new(
        lerp(tip.x, tail.x, 0.22),
        lerp(tip.y, tail.y, 0.22),
        lerp(tip.z, tail.z, 0.22),
    );
    let mut nib = PathBuilder::stroke(px((1.05 * cam.scale).max(0.9)));
    nib.move_to(cam.pt(tip));
    nib.line_to(cam.pt(tip_mid));
    if let Ok(path) = nib.build() {
        window.paint_path(path, tip_col);
    }

    // Tiny pink eraser cap at the tail end
    let (ept, epersp, _) = cam.project(tail);
    let er = (0.78 * epersp * cam.scale).max(0.8);
    window.paint_quad(
        fill(
            Bounds::new(point(ept.x - px(er), ept.y - px(er)), size(px(er * 2.0), px(er * 2.0))),
            eraser_col,
        )
        .corner_radii(px(er)),
    );
}

/// Plan secondary action: subtle rising thought dots while pondering, then a crisp eureka star!
fn paint_plan_thought_fx(
    cam: &Cam3D,
    hy: f32,
    s: f32,
    pondering: bool,
    eureka: f32,
    tint: Hsla,
    window: &mut Window,
) {
    if eureka > 0.1 {
        paint_four_point_star(
            cam.pt(Vec3::new(0.0, -hy - 2.9 - eureka * 0.9, 0.0)),
            (1.5 + eureka * 1.3) * s,
            tint.opacity(eureka),
            window,
        );
        return;
    }
    if pondering {
        let d1 = 1.45 * s;
        let pt1 = cam.pt(Vec3::new(6.4, -hy - 1.4, 0.0));
        window.paint_quad(
            fill(
                Bounds::new(point(pt1.x - px(d1 * 0.5), pt1.y - px(d1 * 0.5)), size(px(d1), px(d1))),
                tint.opacity(0.75),
            )
            .corner_radii(px(d1 * 0.5)),
        );
        let d2 = 1.9 * s;
        let pt2 = cam.pt(Vec3::new(8.0, -hy - 3.2, 0.0));
        window.paint_quad(
            fill(
                Bounds::new(point(pt2.x - px(d2 * 0.5), pt2.y - px(d2 * 0.5)), size(px(d2), px(d2))),
                tint.opacity(0.88),
            )
            .corner_radii(px(d2 * 0.5)),
        );
    }
}

/// Two staggered crisp vector `z`s drifting upward along a curved S-arc while sleeping.
fn paint_sleep_zs(
    ox: f32,
    oy: f32,
    s: f32,
    cycle: f32,
    animate: bool,
    color: Hsla,
    window: &mut Window,
) {
    let offsets: &[f32] = if animate { &[0.0, 0.5] } else { &[0.0] };
    for &offset in offsets {
        let t = (cycle + offset).fract();
        let alpha = if !animate {
            0.75
        } else if t < 0.18 {
            ease_out_cubic(t / 0.18) * 0.88
        } else if t > 0.72 {
            (1.0 - (t - 0.72) / 0.28).powf(1.5) * 0.88
        } else {
            0.88
        };
        if alpha <= 0.02 {
            continue;
        }
        let x = 17.4 + t * 2.5 + (t * std::f32::consts::TAU).sin() * 0.8;
        let y = 7.4 - ease_out_cubic(t) * 5.6;
        let half = (0.95 + t * 0.55) * s;
        let sw = (0.95 * s).max(0.85);

        let cx = ox + x * s;
        let cy = oy + y * s;
        let mut pb = PathBuilder::stroke(px(sw));
        pb.move_to(point(px(cx - half), px(cy - half)));
        pb.line_to(point(px(cx + half), px(cy - half)));
        pb.line_to(point(px(cx - half), px(cy + half)));
        pb.line_to(point(px(cx + half), px(cy + half)));
        if let Ok(path) = pb.build() {
            window.paint_path(path, color.opacity(alpha));
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
// Curated Matte-Vinyl Color Palettes (Dark & Light)
// ---------------------------------------------------------------------------

fn body_palette(role: SubagentRole, state: EntityState, dark: bool, t: Tokens) -> Hsla {
    if state == EntityState::Error {
        return c(t.danger).opacity(0.88);
    }
    if state == EntityState::Cancelled {
        return c(t.text_faint).opacity(0.55);
    }
    let rgba: u32 = match (role, dark) {
        // Explore: cool seafoam teal
        (SubagentRole::Explore, true) => 0x45A8_A6FF,
        (SubagentRole::Explore, false) => 0x3496_94FF,
        // Plan: calm cobalt / periwinkle blue
        (SubagentRole::Plan, true) => 0x6184_E8FF,
        (SubagentRole::Plan, false) => 0x4B6D_D4FF,
        // Worker: warm terracotta coral
        (SubagentRole::Worker, true) => 0xDC73_58FF,
        (SubagentRole::Worker, false) => 0xCC60_44FF,
        // General: fresh sage-emerald
        (SubagentRole::General, true) => 0x52B3_88FF,
        (SubagentRole::General, false) => 0x3E9E_74FF,
        // Custom: curated matte-vinyl hues
        (SubagentRole::Custom(0), true) => 0x9C7A_D4FF,
        (SubagentRole::Custom(0), false) => 0x8662_C0FF,
        (SubagentRole::Custom(1), true) => 0xD48A_A4FF,
        (SubagentRole::Custom(1), false) => 0xC273_90FF,
        (SubagentRole::Custom(2), true) => 0x6CA4_D6FF,
        (SubagentRole::Custom(2), false) => 0x528C_C2FF,
        (SubagentRole::Custom(_), true) => 0xC89B_5EFF,
        (SubagentRole::Custom(_), false) => 0xB485_46FF,
    };
    let mut col = c(rgba);
    if state == EntityState::Done {
        // Slightly calmer, cozier tone while sleeping so active creatures pop!
        col.s *= 0.72;
        col.a = 0.86;
    }
    col
}

fn prop_palette(role: SubagentRole, state: EntityState, dark: bool, t: Tokens) -> Hsla {
    if state == EntityState::Error {
        return adjust_lightness(c(t.danger), 0.10);
    }
    let alpha = match state {
        EntityState::Running | EntityState::Blocked => 1.0,
        EntityState::Done => 0.85,
        EntityState::Cancelled => 0.5,
        EntityState::Error => 0.9,
    };
    let rgba: u32 = match (role, dark) {
        // Worker: classic construction yellow-gold hard hat
        (SubagentRole::Worker, true) => 0xF5BE_3BFF,
        (SubagentRole::Worker, false) => 0xE3A6_1EFF,
        // Explore: warm brass-gold binoculars contrasting with teal box
        (SubagentRole::Explore, true) => 0xE6B1_56FF,
        (SubagentRole::Explore, false) => 0xCF96_36FF,
        // Plan: champagne-gold round glasses contrasting with cobalt box
        (SubagentRole::Plan, true) => 0xF0CA_75FF,
        (SubagentRole::Plan, false) => 0xD6AB_4AFF,
        // General: warm coral-amber orb contrasting with sage box
        (SubagentRole::General, true) => 0xF291_6BFF,
        (SubagentRole::General, false) => 0xE079_52FF,
        // Custom: warm cream/gold accent
        (SubagentRole::Custom(_), true) => 0xEBC9_78FF,
        (SubagentRole::Custom(_), false) => 0xD4AD_50FF,
    };
    c(rgba).opacity(alpha)
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
    fn maps_builtin_and_custom_role_names() {
        assert_eq!(SubagentRole::from_name("explore"), SubagentRole::Explore);
        assert_eq!(
            SubagentRole::from_name("Explore agent"),
            SubagentRole::Explore
        );
        assert_eq!(SubagentRole::from_name("plan"), SubagentRole::Plan);
        assert_eq!(SubagentRole::from_name("worker"), SubagentRole::Worker);
        assert_eq!(SubagentRole::from_name("general"), SubagentRole::General);
        assert_eq!(SubagentRole::from_name("subagent"), SubagentRole::General);
        assert!(matches!(
            SubagentRole::from_name("security-reviewer"),
            SubagentRole::Custom(_)
        ));
    }

    #[test]
    fn animated_states_include_running_blocked_and_sleeping_done() {
        assert!(EntityState::from_status("running").is_animated());
        assert!(EntityState::from_status("blocked").is_animated());
        assert!(EntityState::from_status("done").is_animated());
        assert!(EntityState::from_status("completed").is_animated());
        assert!(!EntityState::from_status("error").is_animated());
        assert!(!EntityState::from_status("cancelled").is_animated());
    }

    #[test]
    fn squash_and_stretch_preserves_volume() {
        for stretch in [-0.16, -0.08, 0.0, 0.08, 0.14] {
            let (sx, sy) = squash_stretch(stretch);
            assert!((sx * sy - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn camera_yaw_depth_sorts_ears_consistently() {
        let cam = Cam3D {
            ox: 0.0,
            oy: 0.0,
            scale: 1.0,
            cx: 12.0,
            cy: 12.0,
            sx: 1.0,
            sy: 1.0,
            yaw: 0.4, // turned right -> left ear swings forward (+z), right ear tucks back (-z)
            pitch: 0.0,
            roll: 0.0,
        };
        let left_z = cam.xform(Vec3::new(-6.5, 0.0, 0.0)).z;
        let right_z = cam.xform(Vec3::new(6.5, 0.0, 0.0)).z;
        assert!(left_z > right_z);
    }
}
