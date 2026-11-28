/* ==========================================================================
   Keel 3D Matte-Vinyl Creature Renderer — Exact 1:1 Canvas Port of creature.rs
   Matches crates/ui/src/creature.rs & crates/proto/src/bots.rs:
     - Cam3D perspective projection (CAM_Z = 44.0)
     - 7-slice extruded matte-vinyl rounded box (hx=5.9, hy=5.4, hz=4.2, r=2.4)
     - Disney 12 Principles kinematics (squash & stretch, anticipation, arcs,
       follow-through hair/mustache lag, irregular double-blinks, eureka star)
   ========================================================================== */

(function () {
  'use strict';

  var TAU = Math.PI * 2;
  var HALF_PI = Math.PI * 0.5;
  var CAM_Z = 44.0;

  var PALETTES = {
    clay:   { dark: 0xDFAF8EFF, light: 0xD39D7AFF, accent: 0xC98862FF },
    coral:  { dark: 0xDC7358FF, light: 0xCC6044FF, accent: 0xE05E4BFF },
    amber:  { dark: 0xE5B24EFF, light: 0xD29C36FF, accent: 0xD98E24FF },
    sage:   { dark: 0x52B388FF, light: 0x3E9E74FF, accent: 0x4E945BFF },
    mint:   { dark: 0x45A8A6FF, light: 0x349694FF, accent: 0x2F9E8BFF },
    sky:    { dark: 0x6184E8FF, light: 0x4B6DD4FF, accent: 0x3482E0FF },
    indigo: { dark: 0x7D7FE6FF, light: 0x6769D4FF, accent: 0x5B5FE0FF },
    plum:   { dark: 0xA87AD8FF, light: 0x9262C4FF, accent: 0x9C57C4FF }
  };

  function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)); }
  function lerp(a, b, t) { return a + (b - a) * t; }
  function remEuclid(a, n) { return ((a % n) + n) % n; }

  function easeInOutCubic(t) {
    t = clamp(t, 0, 1);
    return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) * 0.5;
  }

  function easeOutCubic(t) {
    t = clamp(t, 0, 1);
    return 1 - Math.pow(1 - t, 3);
  }

  function easeOutBack(t) {
    t = clamp(t, 0, 1);
    var c1 = 1.70158, c3 = c1 + 1;
    return 1 + c3 * Math.pow(t - 1, 3) + c1 * Math.pow(t - 1, 2);
  }

  function arc(t) {
    t = clamp(t, 0, 1);
    return 4 * t * (1 - t);
  }

  function squashStretch(stretch) {
    var sy = clamp(1 + stretch, 0.80, 1.22);
    return { sx: 1 / sy, sy: sy };
  }

  function rgbaToHsla(rgba) {
    var r = ((rgba >>> 24) & 0xFF) / 255;
    var g = ((rgba >>> 16) & 0xFF) / 255;
    var b = ((rgba >>> 8) & 0xFF) / 255;
    var a = (rgba & 0xFF) / 255;
    var max = Math.max(r, g, b), min = Math.min(r, g, b);
    var l = (max + min) * 0.5;
    var h = 0, s = 0;
    if (max !== min) {
      var d = max - min;
      s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
      if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
      else if (max === g) h = (b - r) / d + 2;
      else h = (r - g) / d + 4;
      h /= 6;
    }
    return { h: h, s: s, l: l, a: a };
  }

  function adjL(c, delta) {
    return { h: c.h, s: c.s, l: clamp(c.l + delta, 0.05, 0.95), a: c.a };
  }

  function withA(c, alpha) {
    return { h: c.h, s: c.s, l: c.l, a: alpha };
  }

  function css(c) {
    return 'hsla(' + (c.h * 360).toFixed(1) + ',' + (c.s * 100).toFixed(1) + '%,' + (c.l * 100).toFixed(1) + '%,' + c.a.toFixed(3) + ')';
  }

  /* Cam3D */
  function xform(cam, x, y, z) {
    var vx = x * cam.sx, vy = y * cam.sy, vz = z;
    var sr = Math.sin(cam.roll), cr = Math.cos(cam.roll);
    var x1 = vx * cr - vy * sr, y1 = vx * sr + vy * cr;
    var sp = Math.sin(cam.pitch), cp = Math.cos(cam.pitch);
    var y2 = y1 * cp - vz * sp, z2 = y1 * sp + vz * cp;
    var sy = Math.sin(cam.yaw), cy = Math.cos(cam.yaw);
    return { x: x1 * cy + z2 * sy, y: y2, z: -x1 * sy + z2 * cy };
  }

  function project(cam, x, y, z) {
    var r = xform(cam, x, y, z);
    var persp = CAM_Z / Math.max(CAM_Z - r.z, 8.0);
    return {
      x: cam.ox + (cam.cx + r.x * persp) * cam.scale,
      y: cam.oy + (cam.cy + r.y * persp) * cam.scale,
      persp: persp,
      z: r.z
    };
  }

  /* Poses */
  function evalDrKeeleeIdle(time) {
    var u = remEuclid(time, 4.6) / 4.6;
    var breath = Math.sin(time * 1.9);
    var breathLag = Math.sin((time - 0.16) * 1.9);
    var stretch = breath * 0.032;
    var bobY = -breath * 0.28;
    var leanX = 0, yaw = -0.15 + breath * 0.03, pitch = 0.08 - breath * 0.02, roll = 0;
    var gazeX = 0, gazeY = 0, eyeScale = 1.0;
    var hairDx = -breathLag * 0.16, hairDy = -breathLag * 0.22;
    var stacheWiggle = 0, fx = 0, cheer = 0.45;

    if (u > 0.20 && u < 0.54) {
      var k = (u - 0.20) / 0.34;
      var env = k < 0.28 ? easeInOutCubic(k / 0.28) : (k < 0.72 ? 1 : 1 - easeInOutCubic((k - 0.72) / 0.28));
      var lagEnv = k < 0.36 ? easeInOutCubic(clamp(k / 0.36, 0, 1)) : (k < 0.76 ? 1 : 1 - easeInOutCubic(clamp((k - 0.76) / 0.24, 0, 1)));
      yaw = lerp(-0.15, 0.24, env);
      pitch = lerp(0.08, -0.05, env);
      roll = 0.06 * env;
      leanX = 0.35 * env;
      gazeX = 0.65 * env;
      gazeY = -0.20 * env;
      hairDx = (lagEnv - env) * 1.15 - 0.18 * env;
      if (k > 0.32 && k < 0.68) {
        stacheWiggle = Math.sin(((k - 0.32) / 0.36) * TAU * 2) * 0.65;
      }
    } else if (u > 0.68 && u < 0.86) {
      var k2 = (u - 0.68) / 0.18;
      var hop = arc(k2);
      if (k2 < 0.24) {
        stretch -= 0.07 * (k2 / 0.24);
        bobY += 0.4 * (k2 / 0.24);
      } else {
        stretch += 0.075 * hop;
        bobY -= hop * 0.95;
        pitch -= 0.12 * hop;
        eyeScale = 1 + 0.09 * hop;
        hairDy -= hop * 0.55;
        stacheWiggle = hop * 0.55;
        fx = hop * 0.72;
        cheer = 0.85;
      }
    }
    var ss = squashStretch(stretch);
    return { bobY: bobY, leanX: leanX, sx: ss.sx, sy: ss.sy, yaw: yaw, pitch: pitch, roll: roll, gazeX: gazeX, gazeY: gazeY, eyeScale: eyeScale, hairDx: hairDx, hairDy: hairDy, stacheWiggle: stacheWiggle, fx: fx, cheer: cheer };
  }

  function evalBotIdle(time) {
    var u = remEuclid(time, 4.0) / 4.0;
    var breath = Math.sin(time * 1.75);
    var lag = Math.sin((time - 0.16) * 1.75);
    var ss = squashStretch(breath * 0.034);
    var yaw = -0.15 + breath * 0.03, roll = 0, gazeX = 0, hairDx = -lag * 0.14;
    if (u > 0.35 && u < 0.68) {
      var k = (u - 0.35) / 0.33;
      var turn = Math.sin(k * Math.PI);
      yaw = lerp(-0.15, 0.18, easeInOutCubic(turn));
      roll = 0.05 * turn;
      gazeX = 0.55 * turn;
      hairDx = -0.35 * turn;
    }
    return {
      bobY: -breath * 0.30, leanX: breath * 0.12, sx: ss.sx, sy: ss.sy,
      yaw: yaw, pitch: 0.08 - breath * 0.025, roll: roll,
      gazeX: gazeX, gazeY: 0, eyeScale: 1.0,
      hairDx: hairDx, hairDy: -lag * 0.20, stacheWiggle: breath * 0.15, fx: 0, cheer: 0.45
    };
  }

  function evaluatePose(isCreator, time, popAge, animate) {
    if (!animate) {
      return { bobY: 0, leanX: 0, sx: 1, sy: 1, yaw: -0.15, pitch: 0.08, roll: 0, gazeX: 0, gazeY: 0, eyeScale: 1, hairDx: 0, hairDy: 0, stacheWiggle: 0, fx: 0, cheer: 0.4 };
    }
    var pose = isCreator ? evalDrKeeleeIdle(time) : evalBotIdle(time);
    if (popAge !== null && popAge >= 0 && popAge < 0.68) {
      var u = clamp(popAge / 0.66, 0, 1);
      if (u < 0.16) {
        var k = easeInOutCubic(u / 0.16);
        var ss = squashStretch(-0.14 * k);
        pose.bobY += 0.95 * k; pose.sx *= ss.sx; pose.sy *= ss.sy;
        pose.pitch += 0.12 * k; pose.hairDy += 0.35 * k;
      } else if (u < 0.56) {
        var k2 = (u - 0.16) / 0.40;
        var hop = arc(k2);
        var ss2 = squashStretch(0.14 * hop);
        var spin = Math.sin(k2 * TAU);
        pose.bobY += lerp(0.95, 0, easeOutCubic(k2)) - hop * 2.15;
        pose.sx *= ss2.sx; pose.sy *= ss2.sy;
        pose.yaw += spin * 0.42; pose.pitch += -0.16 * hop; pose.roll += -spin * 0.11;
        pose.eyeScale *= 1 + 0.14 * hop;
        pose.hairDx += -spin * 0.95; pose.hairDy += -hop * 0.65;
        pose.stacheWiggle += spin * 0.7;
        pose.fx = Math.max(pose.fx, hop * 0.95);
        pose.cheer = 1.0;
      } else {
        var k3 = (u - 0.56) / 0.44;
        var settle = easeOutBack(k3);
        var damp = Math.pow(1 - k3, 2);
        var ss3 = squashStretch(-0.10 * damp * Math.sin(k3 * Math.PI));
        pose.bobY += 0.45 * damp * (1 - Math.min(settle, 1));
        pose.sx *= ss3.sx; pose.sy *= ss3.sy;
        pose.hairDy += 0.55 * damp;
        pose.stacheWiggle += Math.sin(k3 * TAU) * 0.35 * damp;
        pose.fx = Math.max(pose.fx, (1 - k3) * 0.5);
      }
    }
    return pose;
  }

  function computeBlink(time, animate) {
    if (!animate) return 1.0;
    var cycle = remEuclid(time, 4.4);
    function at(start, dur) {
      if (cycle > start && cycle < start + dur) {
        return 1 - 0.82 * Math.sin(((cycle - start) / dur) * Math.PI);
      }
      return 1.0;
    }
    return Math.min(at(3.75, 0.16), at(4.02, 0.14));
  }

  /* Canvas Primitives */
  function fillPoly(ctx, cam, pts, color) {
    if (!pts.length) return;
    ctx.beginPath();
    var p0 = project(cam, pts[0][0], pts[0][1], pts[0][2]);
    ctx.moveTo(p0.x, p0.y);
    for (var i = 1; i < pts.length; i++) {
      var p = project(cam, pts[i][0], pts[i][1], pts[i][2]);
      ctx.lineTo(p.x, p.y);
    }
    ctx.closePath();
    ctx.fillStyle = css(color);
    ctx.fill();
  }

  function strokeLine(ctx, cam, pts, width, color) {
    if (pts.length < 2) return;
    ctx.beginPath();
    var p0 = project(cam, pts[0][0], pts[0][1], pts[0][2]);
    ctx.moveTo(p0.x, p0.y);
    for (var i = 1; i < pts.length; i++) {
      var p = project(cam, pts[i][0], pts[i][1], pts[i][2]);
      ctx.lineTo(p.x, p.y);
    }
    ctx.strokeStyle = css(color);
    ctx.lineWidth = width;
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    ctx.stroke();
  }

  function fillPill(ctx, cx, cy, w, h, color) {
    var r = Math.min(w, h) * 0.5;
    ctx.beginPath();
    if (ctx.roundRect) {
      ctx.roundRect(cx - w * 0.5, cy - h * 0.5, w, h, r);
    } else {
      ctx.ellipse(cx, cy, w * 0.5, h * 0.5, 0, 0, TAU);
    }
    ctx.fillStyle = css(color);
    ctx.fill();
  }

  function paintRoundedPlane(ctx, cam, hx, hy, radius, zPlane, color) {
    var r = Math.min(radius, hx, hy);
    var corners = [
      [hx - r, -hy + r, -HALF_PI, 0],
      [hx - r, hy - r, 0, HALF_PI],
      [-hx + r, hy - r, HALF_PI, Math.PI],
      [-hx + r, -hy + r, Math.PI, 1.5 * Math.PI]
    ];
    ctx.beginPath();
    var started = false;
    for (var c = 0; c < 4; c++) {
      var cc = corners[c];
      for (var i = 0; i <= 5; i++) {
        var a = lerp(cc[2], cc[3], i / 5);
        var pt = project(cam, cc[0] + r * Math.cos(a), cc[1] + r * Math.sin(a), zPlane);
        if (!started) { ctx.moveTo(pt.x, pt.y); started = true; }
        else ctx.lineTo(pt.x, pt.y);
      }
    }
    ctx.closePath();
    ctx.fillStyle = css(color);
    ctx.fill();
  }

  function paintLowerBoxWrap(ctx, cam, hx, hy, radius, yTop, zPlane, color) {
    var r = Math.min(radius, hx, Math.max(hy - yTop, 0.5));
    ctx.beginPath();
    var pStart = project(cam, -hx, yTop, zPlane);
    var pTopR = project(cam, hx, yTop, zPlane);
    ctx.moveTo(pStart.x, pStart.y);
    ctx.lineTo(pTopR.x, pTopR.y);
    for (var i = 0; i <= 5; i++) {
      var a = lerp(0, HALF_PI, i / 5);
      var pt = project(cam, (hx - r) + r * Math.cos(a), (hy - r) + r * Math.sin(a), zPlane);
      ctx.lineTo(pt.x, pt.y);
    }
    for (var j = 0; j <= 5; j++) {
      var a2 = lerp(HALF_PI, Math.PI, j / 5);
      var pt2 = project(cam, (-hx + r) + r * Math.cos(a2), (hy - r) + r * Math.sin(a2), zPlane);
      ctx.lineTo(pt2.x, pt2.y);
    }
    ctx.closePath();
    ctx.fillStyle = css(color);
    ctx.fill();
  }

  function paintSphere(ctx, cam, x, y, z, radius, base, dark) {
    var p = project(cam, x, y, z);
    var r = radius * p.persp * cam.scale;
    if (r <= 0.3) return;
    fillPill(ctx, p.x, p.y, r * 2, r * 2, adjL(base, -0.12));
    var rl = r * 0.86;
    fillPill(ctx, p.x - r * 0.08, p.y - r * 0.10, rl * 2, rl * 2, base);
    var rs = Math.max(r * 0.30, 0.7);
    fillPill(ctx, p.x - r * 0.36, p.y - r * 0.36, rs * 2, rs * 2, withA(adjL(base, dark ? 0.24 : 0.20), 0.78));
  }

  function paintRoundedBox(ctx, cam, hx, hy, hz, radius, base, dark) {
    for (var i = 0; i < 7; i++) {
      var t = i / 6;
      var z = lerp(-hz, hz * 0.88, t);
      var inset = i === 0 ? 0.35 : 0;
      paintRoundedPlane(ctx, cam, hx - inset, hy - inset, Math.max(radius - inset * 0.5, 0.5), z, adjL(base, lerp(-0.18, -0.07, t)));
    }
    if (cam.pitch > 0.01) {
      var topVis = clamp(cam.pitch * 3.5, 0, 1);
      var ix = hx - radius * 0.45, iz = hz - 0.5;
      fillPoly(ctx, cam, [
        [-ix, -hy + 0.05, -iz], [ix, -hy + 0.05, -iz],
        [ix, -hy + 0.05, iz], [-ix, -hy + 0.05, iz]
      ], withA(adjL(base, dark ? 0.12 : 0.10), 0.65 * topVis));
    }
    paintRoundedPlane(ctx, cam, hx - 0.15, hy - 0.15, radius, hz, adjL(base, dark ? 0.04 : 0.03));
    var sy0 = -hy + 0.85, sy1 = -hy + 2.35, sx0 = -hx + radius * 0.75, sx1 = hx - radius * 0.75;
    fillPoly(ctx, cam, [
      [sx0, sy0, hz + 0.04], [sx1, sy0, hz + 0.04],
      [sx1 - 0.4, sy1, hz + 0.04], [sx0 + 0.4, sy1, hz + 0.04]
    ], withA(adjL(base, dark ? 0.18 : 0.16), 0.38));
  }

  function paintEar(ctx, cam, hx, side, base) {
    var p = project(cam, side * (hx + 0.75), 0.1, 0);
    var rw = 1.9 * p.persp * cam.scale, rh = 3.4 * p.persp * cam.scale;
    fillPill(ctx, p.x, p.y, rw, rh, adjL(base, -0.10));
    fillPill(ctx, p.x, p.y - rh * 0.08, rw * 0.62, rh * 0.65, adjL(base, 0.04));
  }

  function paintAttire(ctx, attire, cam, hx, hy, hz, radius, swayX, dark) {
    var fz = hz + 0.22;
    if (attire === 'doctor') {
      var coatSh = rgbaToHsla(dark ? 0xC8D2E0FF : 0xD3DCE8FF);
      var coatMid = rgbaToHsla(dark ? 0xE6ECF5FF : 0xEDF2F8FF);
      var coatHi = rgbaToHsla(0xFAFCFFFF);
      var yTop = 2.05;
      paintLowerBoxWrap(ctx, cam, hx + 0.16, hy + 0.14, radius, yTop, -hz * 0.55, coatSh);
      paintLowerBoxWrap(ctx, cam, hx + 0.16, hy + 0.14, radius, yTop, 0, adjL(coatMid, -0.05));
      paintLowerBoxWrap(ctx, cam, hx + 0.16, hy + 0.14, radius, yTop, hz * 0.65, coatMid);
      paintLowerBoxWrap(ctx, cam, hx + 0.16, hy + 0.14, radius, yTop, hz + 0.14, coatHi);
      fillPoly(ctx, cam, [[-2.15, yTop - 0.05, fz], [2.15, yTop - 0.05, fz], [0, hy - 0.55, fz]], rgbaToHsla(0x286678FF));
      var lapel = rgbaToHsla(0xFFFFFFFF);
      [-1, 1].forEach(function (side) {
        fillPoly(ctx, cam, [
          [side * 2.15, yTop - 0.08, fz + 0.06], [side * 3.25, yTop + 0.85, fz + 0.06],
          [side * 1.95, yTop + 1.85, fz + 0.06], [side * 0.12, hy - 0.65, fz + 0.06]
        ], lapel);
      });
      strokeLine(ctx, cam, [[0, hy - 0.65, fz + 0.06], [0, hy + 0.08, fz + 0.06]], Math.max(0.7 * cam.scale, 0.7), withA(rgbaToHsla(0x94A3B8FF), 0.8));
      fillPoly(ctx, cam, [[2.1, 3.35, fz + 0.08], [4.3, 3.35, fz + 0.08], [4.1, 4.75, fz + 0.08], [2.2, 4.75, fz + 0.08]], rgbaToHsla(0xDCE4F0FF));
      var penSw = Math.max(0.85 * cam.scale, 0.8);
      strokeLine(ctx, cam, [[2.8, 2.75, fz + 0.10], [2.8, 3.55, fz + 0.10]], penSw, rgbaToHsla(0x2563EBFF));
      strokeLine(ctx, cam, [[3.55, 2.75, fz + 0.10], [3.55, 3.55, fz + 0.10]], penSw, rgbaToHsla(0xE11D48FF));
      strokeLine(ctx, cam, [
        [-3.2, yTop - 0.15, fz + 0.12], [-2.6 + swayX * 0.15, 3.65, fz + 0.20],
        [-0.7 + swayX * 0.20, 4.15, fz + 0.22], [1.5 + swayX * 0.15, 3.60, fz + 0.20],
        [2.7, yTop - 0.15, fz + 0.12]
      ], Math.max(0.72 * cam.scale, 1.0), rgbaToHsla(0x334155FF));
      var cp = project(cam, -2.35 + swayX * 0.22, 4.25, fz + 0.30);
      var cr = 0.88 * cp.persp * cam.scale;
      fillPill(ctx, cp.x, cp.y, cr * 2, cr * 2, rgbaToHsla(0x94A3B8FF));
      fillPill(ctx, cp.x, cp.y, cr * 1.24, cr * 1.24, rgbaToHsla(0x334155FF));
    } else if (attire === 'hoodie') {
      var base = rgbaToHsla(dark ? 0x3D465BFF : 0x343C4FFF);
      paintLowerBoxWrap(ctx, cam, hx + 0.18, hy + 0.14, radius, 2.05, -hz * 0.5, adjL(base, -0.12));
      paintLowerBoxWrap(ctx, cam, hx + 0.18, hy + 0.14, radius, 2.05, 0, adjL(base, -0.05));
      paintLowerBoxWrap(ctx, cam, hx + 0.18, hy + 0.14, radius, 2.05, hz + 0.14, adjL(base, 0.04));
      var dsw = Math.max(0.85 * cam.scale, 0.8);
      [-1, 1].forEach(function (side) {
        strokeLine(ctx, cam, [[side * 1.35, 2.15, fz], [side * 1.45 + swayX * 0.2, 4.20, fz + 0.08]], dsw, rgbaToHsla(0xE2E8F0FF));
      });
      fillPoly(ctx, cam, [[-2.7, 3.8, fz], [2.7, 3.8, fz], [2.2, 5.15, fz], [-2.2, 5.15, fz]], adjL(base, -0.08));
    } else if (attire === 'suit') {
      var jacket = rgbaToHsla(dark ? 0x263144FF : 0x1E293BFF);
      paintLowerBoxWrap(ctx, cam, hx + 0.16, hy + 0.14, radius, 2.05, -hz * 0.5, adjL(jacket, -0.10));
      paintLowerBoxWrap(ctx, cam, hx + 0.16, hy + 0.14, radius, 2.05, hz + 0.14, adjL(jacket, 0.04));
      fillPoly(ctx, cam, [[-2.0, 2.0, fz], [2.0, 2.0, fz], [0, hy - 0.2, fz]], rgbaToHsla(0xFFFFFFFF));
      fillPoly(ctx, cam, [[-0.55, 2.15, fz + 0.06], [0.55, 2.15, fz + 0.06], [0.75, hy - 0.9, fz + 0.06], [0, hy - 0.1, fz + 0.06], [-0.75, hy - 0.9, fz + 0.06]], rgbaToHsla(0xDC2626FF));
    } else if (attire === 'overalls') {
      var denim = rgbaToHsla(0x3B6EA5FF);
      paintLowerBoxWrap(ctx, cam, hx + 0.15, hy + 0.14, radius, 3.05, -hz * 0.45, adjL(denim, -0.12));
      paintLowerBoxWrap(ctx, cam, hx + 0.15, hy + 0.14, radius, 3.05, hz + 0.14, adjL(denim, 0.04));
      [-1, 1].forEach(function (side) {
        fillPoly(ctx, cam, [[side * 3.1, 1.85, fz - 0.02], [side * 1.9, 1.85, fz - 0.02], [side * 1.9, 3.45, fz - 0.02], [side * 3.1, 3.45, fz - 0.02]], adjL(denim, 0.05));
        paintSphere(ctx, cam, side * 2.5, 3.4, fz + 0.13, 0.62, rgbaToHsla(0xFBBF24FF), dark);
      });
    } else if (attire === 'cape') {
      strokeLine(ctx, cam, [[-hx + 0.2, 2.25, fz - 0.15], [0, 2.85, fz + 0.12], [hx - 0.2, 2.25, fz - 0.15]], Math.max(1.35 * cam.scale, 1.1), rgbaToHsla(0xC83A4AFF));
      paintSphere(ctx, cam, 0, 2.85, fz + 0.25, 0.95, rgbaToHsla(0xF5B938FF), dark);
    } else if (attire === 'sweater') {
      var knit = rgbaToHsla(0xF0E2CCFF);
      paintLowerBoxWrap(ctx, cam, hx + 0.16, hy + 0.14, radius, 2.05, -hz * 0.5, adjL(knit, -0.10));
      paintLowerBoxWrap(ctx, cam, hx + 0.16, hy + 0.14, radius, 2.05, hz + 0.14, adjL(knit, 0.03));
      strokeLine(ctx, cam, [[-hx + 0.2, 3.65, fz], [hx - 0.2, 3.65, fz]], Math.max(1.65 * cam.scale, 1.2), rgbaToHsla(0xD97757FF));
    }
  }

  function paintRoundGlasses(ctx, cam, hx, z, y, tint) {
    var r = 2.25, sw = Math.max(0.52 * cam.scale, 1.0);
    [-2.42, 2.42].forEach(function (cx) {
      ctx.beginPath();
      for (var i = 0; i <= 20; i++) {
        var a = (i / 20) * TAU;
        var pt = project(cam, cx + r * Math.cos(a), y + r * Math.sin(a), z);
        if (i === 0) ctx.moveTo(pt.x, pt.y); else ctx.lineTo(pt.x, pt.y);
      }
      ctx.closePath();
      ctx.fillStyle = 'rgba(255,255,255,0.12)';
      ctx.fill();
      ctx.strokeStyle = css(tint);
      ctx.lineWidth = sw;
      ctx.stroke();
      strokeLine(ctx, cam, [[cx - 1.15, y - 0.55, z + 0.04], [cx - 0.45, y - 1.25, z + 0.04]], Math.max(0.36 * cam.scale, 0.75), withA(rgbaToHsla(0xFFFFFFFF), 0.45));
    });
    strokeLine(ctx, cam, [[-2.42 + r - 0.08, y - 0.12, z], [2.42 - r + 0.08, y - 0.12, z]], sw * 0.92, tint);
    strokeLine(ctx, cam, [[-2.42 - r, y - 0.22, z], [-hx - 0.1, y - 0.48, 0.4]], sw * 0.85, withA(tint, 0.88));
    strokeLine(ctx, cam, [[2.42 + r, y - 0.22, z], [hx + 0.1, y - 0.48, 0.4]], sw * 0.85, withA(tint, 0.88));
  }

  function paintEyes(ctx, eyes, cam, hx, faceZ, gazeX, gazeY, eyeScale, blink, eyeCol, catchlight, dark) {
    var eyeY = -0.75 + gazeY * 0.85;
    var xs = [-2.35 + gazeX * 0.75, 2.35 + gazeX * 0.75];
    var yf = Math.max(Math.cos(cam.yaw), 0.65);

    if (eyes === 'dot' || eyes === 'specs') {
      var eh = Math.max(2.35 * eyeScale * blink, 0.65);
      xs.forEach(function (cx) {
        var p = project(cam, cx, eyeY, faceZ);
        var pw = 1.95 * yf * p.persp * cam.scale, ph = eh * p.persp * cam.scale;
        fillPill(ctx, p.x, p.y, pw, ph, eyeCol);
        if (blink > 0.65) {
          var cd = Math.max(0.76 * p.persp * cam.scale, 0.85);
          fillPill(ctx, p.x - pw * 0.22, p.y - ph * 0.22, cd, cd, catchlight);
        }
      });
      if (eyes === 'specs') {
        paintRoundGlasses(ctx, cam, hx, faceZ + 0.62, eyeY, rgbaToHsla(dark ? 0xE8C874FF : 0x3D4656FF));
      }
    } else if (eyes === 'wide') {
      xs.forEach(function (cx) {
        var p = project(cam, cx, eyeY, faceZ);
        var rw = 2.75 * yf * p.persp * cam.scale;
        var rh = Math.max(3.05 * eyeScale * blink, 0.85) * p.persp * cam.scale;
        fillPill(ctx, p.x, p.y, rw, rh, rgbaToHsla(0xFFFFFFFF));
        fillPill(ctx, p.x, p.y, rw * 0.66, rh * 0.68, eyeCol);
        if (blink > 0.65) {
          var cd = Math.max(0.85 * p.persp * cam.scale, 0.9);
          fillPill(ctx, p.x - rw * 0.16, p.y - rh * 0.16, cd, cd, catchlight);
        }
      });
    } else if (eyes === 'sleepy') {
      var sw = Math.max(1.15 * cam.scale, 1.0);
      xs.forEach(function (cx) {
        var p = project(cam, cx, eyeY + 0.25, faceZ);
        fillPill(ctx, p.x, p.y, 1.95 * yf * p.persp * cam.scale, 1.35 * blink * p.persp * cam.scale, eyeCol);
        strokeLine(ctx, cam, [[cx - 1.25, eyeY - 0.35, faceZ + 0.06], [cx + 1.25, eyeY - 0.20, faceZ + 0.06]], sw, eyeCol);
      });
    } else if (eyes === 'shades') {
      var z = faceZ + 0.68, dg = rgbaToHsla(0x161922FF);
      [-2.45, 2.45].forEach(function (cx) {
        fillPoly(ctx, cam, [[cx - 2.15, eyeY - 1.45, z], [cx + 2.15, eyeY - 1.45, z], [cx + 1.85, eyeY + 1.55, z], [cx - 1.85, eyeY + 1.55, z]], dg);
        strokeLine(ctx, cam, [[cx - 1.15, eyeY + 0.6, z + 0.06], [cx + 0.35, eyeY - 0.85, z + 0.06]], Math.max(0.95 * cam.scale, 0.85), withA(rgbaToHsla(0xFFFFFFFF), 0.42));
      });
      var bsw = Math.max(1.15 * cam.scale, 1.0);
      strokeLine(ctx, cam, [[-0.45, eyeY - 0.75, z], [0.45, eyeY - 0.75, z]], bsw, dg);
      strokeLine(ctx, cam, [[-4.5, eyeY - 0.7, z], [-hx - 0.1, eyeY - 0.8, 0.4]], bsw * 0.85, dg);
      strokeLine(ctx, cam, [[4.5, eyeY - 0.7, z], [hx + 0.1, eyeY - 0.8, 0.4]], bsw * 0.85, dg);
    } else if (eyes === 'wink') {
      var p = project(cam, xs[0], eyeY, faceZ);
      var pw = 2.0 * yf * p.persp * cam.scale, ph = Math.max(2.45 * eyeScale * blink, 0.65) * p.persp * cam.scale;
      fillPill(ctx, p.x, p.y, pw, ph, eyeCol);
      if (blink > 0.65) {
        var cd = Math.max(0.78 * p.persp * cam.scale, 0.85);
        fillPill(ctx, p.x - pw * 0.22, p.y - ph * 0.22, cd, cd, catchlight);
      }
      strokeLine(ctx, cam, [[xs[1] - 1.1, eyeY + 0.25, faceZ], [xs[1], eyeY - 0.65, faceZ], [xs[1] + 1.1, eyeY + 0.25, faceZ]], Math.max(1.2 * cam.scale, 1.0), eyeCol);
    }
  }

  function paintMustache(ctx, mustache, cam, z, wiggle, dark) {
    var my = 1.05, dy = wiggle * 0.22;
    if (mustache === 'einstein') {
      var base = rgbaToHsla(dark ? 0xF0F3F8FF : 0xF7F9FCFF);
      var sh = rgbaToHsla(dark ? 0xB8C2D4FF : 0xC4CDDDFF);
      var bristle = withA(rgbaToHsla(dark ? 0x9AA5B8FF : 0xAEB8C9FF), 0.62);
      function walrus(zOff, yOff, col) {
        fillPoly(ctx, cam, [
          [0, my - 0.18 + yOff, zOff], [1.35, my - 0.08 + yOff, zOff],
          [2.75, my + 0.32 + yOff - dy * 0.5, zOff], [3.95, my + 0.92 + yOff - dy, zOff],
          [4.25, my + 1.55 + yOff - dy, zOff], [3.35, my + 1.48 + yOff - dy * 0.8, zOff],
          [2.95, my + 1.92 + yOff - dy * 0.6, zOff], [1.95, my + 1.62 + yOff - dy * 0.4, zOff],
          [1.15, my + 1.88 + yOff - dy * 0.2, zOff], [0, my + 1.38 + yOff, zOff],
          [-1.15, my + 1.88 + yOff + dy * 0.2, zOff], [-1.95, my + 1.62 + yOff + dy * 0.4, zOff],
          [-2.95, my + 1.92 + yOff + dy * 0.6, zOff], [-3.35, my + 1.48 + yOff + dy * 0.8, zOff],
          [-4.25, my + 1.55 + yOff + dy, zOff], [-3.95, my + 0.92 + yOff + dy, zOff],
          [-2.75, my + 0.32 + yOff + dy * 0.5, zOff], [-1.35, my - 0.08 + yOff, zOff]
        ], col);
      }
      walrus(z - 0.12, 0.24, sh);
      walrus(z + 0.14, 0, base);
      var bsw = Math.max(0.36 * cam.scale, 0.65);
      [[-2.3, -2.55], [-1.1, -1.2], [1.1, 1.2], [2.3, 2.55]].forEach(function (pair) {
        strokeLine(ctx, cam, [[pair[0], my + 0.45, z + 0.18], [pair[1], my + 1.35, z + 0.18]], bsw, bristle);
      });
    } else if (mustache === 'handlebar') {
      var col = rgbaToHsla(0x3B2B23FF), sw = Math.max(1.35 * cam.scale, 1.1);
      [-1, 1].forEach(function (side) {
        strokeLine(ctx, cam, [[0, my + 0.15, z], [side * 1.8, my + 0.65, z + 0.08], [side * 3.25, my + 0.55 + side * dy, z + 0.12], [side * 3.55, my - 0.25 + side * dy, z + 0.14]], sw, col);
      });
    }
  }

  function paintEinsteinBack(ctx, cam, hx, hy, hz, lx, ly, dark) {
    var sBack = rgbaToHsla(dark ? 0xC6CFE0FF : 0xD2DAE8FF);
    var sMid = rgbaToHsla(dark ? 0xDFE5F0FF : 0xE8EDF6FF);
    [-1, 1].forEach(function (side) {
      [[-hz * 0.55, 1.06, sBack], [-0.4, 1.0, sMid]].forEach(function (layer) {
        var zOff = layer[0], sc = layer[1], col = layer[2];
        fillPoly(ctx, cam, [
          [side * (hx - 2.6), -hy - 0.1, zOff],
          [side * (hx + 0.9 * sc) + lx, -hy - 2.65 * sc + ly, zOff],
          [side * (hx + 0.6 * sc) + lx * 0.7, -hy - 0.85 + ly * 0.6, zOff],
          [side * (hx + 4.05 * sc) + lx * 1.15, -hy - 1.35 * sc + ly * 0.85, zOff],
          [side * (hx + 2.15 * sc) + lx * 0.85, -hy + 0.65 + ly * 0.5, zOff],
          [side * (hx + 4.35 * sc) + lx * 1.1, -hy + 1.45 + ly * 0.65, zOff],
          [side * (hx + 2.05 * sc) + lx * 0.75, -hy + 2.65 + ly * 0.4, zOff],
          [side * (hx + 3.45 * sc) + lx * 0.95, -hy + 4.05 + ly * 0.45, zOff],
          [side * (hx + 0.65), -hy + 4.95, zOff],
          [side * (hx - 1.2), -hy + 4.2, zOff]
        ], col);
      });
    });
    fillPoly(ctx, cam, [
      [-3.8, -hy + 0.2, -hz * 0.35], [-2.9 + lx * 0.8, -hy - 2.05 + ly, -hz * 0.35],
      [-1.3 + lx * 0.5, -hy - 0.65 + ly * 0.5, -hz * 0.35], [0.2 + lx * 0.9, -hy - 2.35 + ly, -hz * 0.35],
      [1.5 + lx * 0.6, -hy - 0.75 + ly * 0.5, -hz * 0.35], [3.1 + lx * 0.8, -hy - 1.95 + ly, -hz * 0.35],
      [3.8, -hy + 0.2, -hz * 0.35]
    ], sMid);
  }

  function paintHairFront(ctx, hair, cam, hx, hy, hz, bodyColor, lx, ly, dark) {
    if (hair === 'einstein') {
      var white = rgbaToHsla(dark ? 0xF2F5FAFF : 0xFAFBFDFF);
      var strand = withA(rgbaToHsla(dark ? 0xBCC6D8FF : 0xCBD3E2FF), 0.68);
      var ssw = Math.max(0.38 * cam.scale, 0.7);
      var lz = xform(cam, -hx - 1.2, -hy + 1.5, 1.4).z;
      var rz = xform(cam, hx + 1.2, -hy + 1.5, 1.4).z;
      (lz < rz ? [-1, 1] : [1, -1]).forEach(function (side) {
        var zf = 1.65;
        fillPoly(ctx, cam, [
          [side * (hx - 1.85), -hy + 0.15, zf],
          [side * (hx + 1.85) + lx, -hy - 1.85 + ly, zf],
          [side * (hx + 1.05) + lx * 0.7, -hy - 0.15 + ly * 0.6, zf],
          [side * (hx + 3.45) + lx * 1.15, -hy + 0.35 + ly * 0.75, zf],
          [side * (hx + 1.45) + lx * 0.75, -hy + 1.85 + ly * 0.45, zf],
          [side * (hx + 2.85) + lx * 0.95, -hy + 3.15 + ly * 0.4, zf],
          [side * (hx + 0.15), -hy + 3.65, zf],
          [side * (hx - 0.45), -hy + 1.65, zf]
        ], white);
        strokeLine(ctx, cam, [[side * (hx - 0.4), -hy + 0.95, zf + 0.08], [side * (hx + 2.25) + lx * 0.9, -hy + 0.55 + ly * 0.6, zf + 0.08]], ssw, strand);
      });
      var wsw = Math.max(0.46 * cam.scale, 0.85);
      strokeLine(ctx, cam, [[-1.8, -hy + 0.1, 1.2], [-2.4 + lx * 0.9, -hy - 1.45 + ly, 1.3], [-3.2 + lx * 1.2, -hy - 2.15 + ly * 1.1, 1.35]], wsw, white);
      strokeLine(ctx, cam, [[2.1, -hy + 0.1, 1.2], [2.9 + lx * 0.9, -hy - 1.35 + ly, 1.3], [3.7 + lx * 1.2, -hy - 1.85 + ly * 1.1, 1.35]], wsw, white);
    } else if (hair === 'tuft') {
      strokeLine(ctx, cam, [[0, -hy + 0.2, 0.2], [0.4 + lx * 0.6, -hy - 2.3 + ly, 0.35]], Math.max(1.35 * cam.scale, 1.0), adjL(bodyColor, -0.10));
      paintSphere(ctx, cam, -0.8 + lx, -hy - 2.75 + ly, 0.45, 1.35, adjL(bodyColor, 0.08), dark);
      paintSphere(ctx, cam, 1.3 + lx, -hy - 2.35 + ly, 0.45, 1.15, adjL(bodyColor, 0.08), dark);
    } else if (hair === 'spiky') {
      var col = rgbaToHsla(0x2B303CFF);
      [[-3.4, -hy - 2.4, 0.4], [-1.1, -hy - 3.3, 0.9], [1.4, -hy - 3.1, 0.9], [3.5, -hy - 2.2, 0.4]].forEach(function (pk) {
        fillPoly(ctx, cam, [[pk[0] - 1.5, -hy + 0.45, pk[2]], [pk[0] + lx * 0.6, pk[1] + ly * 0.7, pk[2] + 0.35], [pk[0] + 1.5, -hy + 0.45, pk[2]]], col);
      });
    } else if (hair === 'bob') {
      fillPoly(ctx, cam, [
        [-hx - 0.35, -hy + 2.4, hz + 0.25], [-hx + 0.2, -hy - 0.55, hz * 0.4],
        [0, -hy - 1.15, hz * 0.4], [hx - 0.2, -hy - 0.55, hz * 0.4],
        [hx + 0.35, -hy + 2.4, hz + 0.25], [1.8, -hy + 1.4, hz + 0.30], [-2.2, -hy + 1.8, hz + 0.30]
      ], rgbaToHsla(0x3B2E36FF));
    } else if (hair === 'mohawk') {
      var mBase = rgbaToHsla(0xE11D48FF);
      [-2.2, -0.6, 1.0, 2.4].forEach(function (z, ix) {
        var h = (ix === 1 || ix === 2) ? 3.5 : 2.7;
        paintSphere(ctx, cam, lx * 0.65, -hy - h * 0.52 + ly * 0.6, z, 1.55, adjL(mBase, (ix - 1.5) * 0.04), dark);
      });
    } else if (hair === 'beanie') {
      var knit = rgbaToHsla(0xDE5847FF), cuff = rgbaToHsla(0xC24334FF);
      var by = -hy + 0.45 + ly * 0.35;
      [[-hz * 0.55, 0.95, -0.14], [0, 1.0, -0.04], [hz * 0.55, 0.97, 0.05]].forEach(function (l) {
        var pts = [];
        for (var i = 0; i <= 14; i++) {
          var a = (i / 14) * Math.PI;
          pts.push([lx * 0.35 + (hx + 0.25) * l[1] * Math.cos(a), by - 3.8 * l[1] * Math.pow(Math.max(Math.sin(a), 0), 0.86), l[0]]);
        }
        fillPoly(ctx, cam, pts, adjL(knit, l[2]));
      });
      fillPoly(ctx, cam, [[-hx - 0.45, by - 0.75, hz + 0.22], [hx + 0.45, by - 0.75, hz + 0.22], [hx + 0.45, by + 0.95, hz + 0.25], [-hx - 0.45, by + 0.95, hz + 0.25]], cuff);
      paintSphere(ctx, cam, lx * 0.95, by - 4.35 + ly * 0.9, 0.2, 1.55, rgbaToHsla(0xF5E7D8FF), dark);
    }
  }

  function paintStar(ctx, pt, r, color) {
    var inner = r * 0.32;
    ctx.beginPath();
    ctx.moveTo(pt.x, pt.y - r);
    ctx.lineTo(pt.x + inner, pt.y - inner);
    ctx.lineTo(pt.x + r, pt.y);
    ctx.lineTo(pt.x + inner, pt.y + inner);
    ctx.lineTo(pt.x, pt.y + r);
    ctx.lineTo(pt.x - inner, pt.y + inner);
    ctx.lineTo(pt.x - r, pt.y);
    ctx.lineTo(pt.x - inner, pt.y - inner);
    ctx.closePath();
    ctx.fillStyle = css(color);
    ctx.fill();
  }

  function paintBot3D(ctx, w, h, cfg, elapsed, dark, animate) {
    var vp = 21.5;
    var s = (Math.min(w, h) / vp) * (cfg.sizeScale || 1.0);
    var ox = (w - vp * s) * 0.5;
    var oy = (h - vp * s) * 0.5;
    var time = elapsed + (cfg.phase || 0) * TAU;
    var popAge = cfg.popAt ? (performance.now() - cfg.popAt) / 1000 : null;
    if (popAge !== null && popAge >= 0.68) { cfg.popAt = null; popAge = null; }

    var pose = evaluatePose(!!cfg.isCreator, time, popAge, animate);
    var hx = 5.9, hy = 5.4, hz = 4.2, radius = 2.4;
    var cam = {
      ox: ox, oy: oy, scale: s,
      cx: vp * 0.5 + pose.leanX, cy: 12.1 + pose.bobY,
      sx: pose.sx, sy: pose.sy, yaw: pose.yaw, pitch: pose.pitch, roll: pose.roll
    };

    var pal = PALETTES[cfg.color] || PALETTES.clay;
    var bodyCol = rgbaToHsla(dark ? pal.dark : pal.light);
    var eyeCol = rgbaToHsla(dark ? 0x111216FF : 0x1C1B1AFF);
    var catchlight = withA(rgbaToHsla(0xFFFFFFFF), 0.95);

    // 1. Dynamic floor shadow
    var lift = Math.max(-pose.bobY, 0) / 2.2;
    var sw = Math.max(12.4 - lift * 3.6, 5.5), sh = Math.max(2.15 - lift * 0.55, 1.0);
    var sa = (dark ? 0.36 : 0.15) * (1 - lift * 0.42);
    fillPill(ctx, ox + (vp * 0.5 + pose.leanX * 0.55) * s, oy + 19.2 * s, sw * s, sh * s, withA(rgbaToHsla(0x000000FF), sa));

    // 2. Back layers
    if (cfg.attire === 'cape') {
      var capeCol = rgbaToHsla(dark ? 0xC83A4AFF : 0xB82E3EFF);
      fillPoly(ctx, cam, [
        [-hx + 0.4, 1.5, -hz * 0.4], [hx - 0.4, 1.5, -hz * 0.4],
        [hx + 2.4 + pose.hairDx * 0.7, hy + 1.8 + pose.hairDy * 0.3, -hz - 1.2],
        [pose.hairDx * 0.8, hy + 2.2 + pose.hairDy * 0.3, -hz - 1.5],
        [-hx - 2.4 + pose.hairDx * 0.7, hy + 1.8 + pose.hairDy * 0.3, -hz - 1.2]
      ], adjL(capeCol, -0.14));
      fillPoly(ctx, cam, [
        [-hx + 0.6, 1.8, -hz * 0.2], [hx - 0.6, 1.8, -hz * 0.2],
        [hx + 1.8 + pose.hairDx * 0.5, hy + 1.2 + pose.hairDy * 0.2, -hz - 0.6],
        [-hx - 1.8 + pose.hairDx * 0.5, hy + 1.2 + pose.hairDy * 0.2, -hz - 0.6]
      ], capeCol);
    }
    if (cfg.hair === 'einstein') paintEinsteinBack(ctx, cam, hx, hy, hz, pose.hairDx, pose.hairDy, dark);
    else if (cfg.hair === 'bob') paintRoundedPlane(ctx, cam, hx + 1.05, hy * 0.85, 3.0, -hz * 0.45, rgbaToHsla(0x2F252DFF));

    // 3. Far ear
    var lez = xform(cam, -(hx + 0.75), 0.1, 0).z;
    var rez = xform(cam, hx + 0.75, 0.1, 0).z;
    if (lez < 0) paintEar(ctx, cam, hx, -1, bodyCol);
    if (rez < 0) paintEar(ctx, cam, hx, 1, bodyCol);

    // 4. 3D Matte-Vinyl Rounded Box Body
    paintRoundedBox(ctx, cam, hx, hy, hz, radius, bodyCol, dark);

    // 5. Near ear
    if (lez >= 0) paintEar(ctx, cam, hx, -1, bodyCol);
    if (rez >= 0) paintEar(ctx, cam, hx, 1, bodyCol);

    // 6. 3D Attire
    paintAttire(ctx, cfg.attire || 'naked', cam, hx, hy, hz, radius, pose.hairDx, dark);

    // 7. Expressive 3D Face
    var faceZ = hz + 0.14;
    var blink = computeBlink(time, animate);
    paintEyes(ctx, cfg.eyes || 'dot', cam, hx, faceZ, pose.gazeX, pose.gazeY, pose.eyeScale, blink, eyeCol, catchlight, dark);

    if (cfg.mustache !== 'einstein') {
      var dip = 0.35 + pose.cheer * 0.35, dx = pose.gazeX * 0.35;
      strokeLine(ctx, cam, [[dx - 1.05, 1.30, faceZ], [dx, 1.45 + dip, faceZ], [dx + 1.05, 1.30, faceZ]], Math.max(0.95 * cam.scale, 0.85), withA(eyeCol, 0.82));
    }

    // 8. Mustache
    paintMustache(ctx, cfg.mustache || 'none', cam, faceZ + 0.18, pose.stacheWiggle, dark);

    // 9. Hair / Headwear
    paintHairFront(ctx, cfg.hair || 'none', cam, hx, hy, hz, bodyCol, pose.hairDx, pose.hairDy, dark);

    // 10. Eureka star / pop sparkle
    if (animate && pose.fx > 0.14) {
      var starCol = cfg.isCreator ? rgbaToHsla(0xF5C84CFF) : adjL(rgbaToHsla(pal.accent), 0.18);
      var sp = project(cam, cfg.isCreator ? 3.6 : 0, -hy - 3.0 - pose.fx * 1.2, 0.4);
      paintStar(ctx, sp, (1.35 + pose.fx * 1.35) * s, withA(starCol, clamp(pose.fx, 0, 1) * 0.92));
      if (cfg.isCreator && pose.fx > 0.35) {
        var sp2 = project(cam, -4.8, -hy - 2.2 - pose.fx * 0.7, 0.2);
        paintStar(ctx, sp2, (0.75 + pose.fx * 0.65) * s, withA(rgbaToHsla(0x7AE5D8FF), pose.fx * 0.8));
      }
    }
  }

  /* Registry & Animation Loop */
  var instances = [];
  var startTime = performance.now();
  var reduced = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  function setupCanvas(canvas) {
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    var rect = canvas.getBoundingClientRect();
    var w = Math.round(rect.width) || parseInt(canvas.getAttribute('width'), 10) || 120;
    var h = Math.round(rect.height) || parseInt(canvas.getAttribute('height'), 10) || 120;
    if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
      canvas.width = w * dpr;
      canvas.height = h * dpr;
    }
    var ctx = canvas.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return { ctx: ctx, w: w, h: h };
  }

  function renderAll(now) {
    var elapsed = (now - startTime) / 1000;
    var dark = document.documentElement.getAttribute('data-theme') === 'dark';
    for (var i = 0; i < instances.length; i++) {
      var inst = instances[i];
      if (!inst.canvas.isConnected) continue;
      // Skip hidden elements to keep 60fps effortless
      if (inst.canvas.offsetParent === null) continue;
      var c = setupCanvas(inst.canvas);
      c.ctx.clearRect(0, 0, c.w, c.h);
      paintBot3D(c.ctx, c.w, c.h, inst.cfg, elapsed, dark, !reduced);
    }
    if (!reduced) {
      requestAnimationFrame(renderAll);
    }
  }

  function mount(canvas, cfg) {
    var inst = { canvas: canvas, cfg: cfg };
    instances.push(inst);
    var c = setupCanvas(canvas);
    var dark = document.documentElement.getAttribute('data-theme') === 'dark';
    paintBot3D(c.ctx, c.w, c.h, cfg, 0, dark, !reduced);
    return inst;
  }

  function init() {
    document.querySelectorAll('canvas[data-creature]').forEach(function (el, idx) {
      var cfg = {
        isCreator: el.getAttribute('data-creator') === 'true',
        color: el.getAttribute('data-color') || 'clay',
        attire: el.getAttribute('data-attire') || 'naked',
        eyes: el.getAttribute('data-eyes') || 'dot',
        hair: el.getAttribute('data-hair') || 'none',
        mustache: el.getAttribute('data-mustache') || 'none',
        sizeScale: parseFloat(el.getAttribute('data-scale') || '0.96'),
        phase: (idx * 0.29) % 1,
        popAt: null
      };
      el._keelCreature = mount(el, cfg);
    });
    if (!reduced) {
      requestAnimationFrame(renderAll);
    }
  }

  window.KeelCreatures = {
    init: init,
    update: function (canvas, patch) {
      if (!canvas || !canvas._keelCreature) return;
      var cfg = canvas._keelCreature.cfg;
      Object.keys(patch).forEach(function (k) { cfg[k] = patch[k]; });
      cfg.popAt = performance.now();
      if (reduced) {
        var c = setupCanvas(canvas);
        var dark = document.documentElement.getAttribute('data-theme') === 'dark';
        c.ctx.clearRect(0, 0, c.w, c.h);
        paintBot3D(c.ctx, c.w, c.h, cfg, 0, dark, false);
      }
    },
    redrawStatic: function () {
      if (reduced) renderAll(performance.now());
    }
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
