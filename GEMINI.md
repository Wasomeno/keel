# Product North Star: Modern, Efficient, Sleek UI/UX & Motion

Keel's core design ethos is: **"A modern app that is blazing fast and efficient, with sleek, tactile UI/UX and fluid motion."**

Every interaction must balance two non-negotiable pillars:
1. **Uncompromised Efficiency**: Zero perceived latency, instant response on pointer-down, snappy keyboard workflows, and motion that never slows the user down (under 200ms).
2. **Sleek Craft & Tactile Motion**: Interfaces that behave like physical, refined materials under the cursor (springs, squash-and-stretch, momentum, scale blooms, concentric curvature).

---

## 1. Role & Mandate
Operate as a **Lead Design Engineer**, not a ticket patcher. You have full ownership and mandate to design complete micro-interactions, tactile press feedback, fluid transitions, and refined visual geometry. Never deliver a bare-minimum static patch or wait to be asked for baseline polish.

---

## 2. The Efficiency Principle (Speed Without Sluggishness)
- **Zero Input Latency**: Respond on pointer-down, not release. Never delay user action for decorative animations.
- **Strict Motion Budgets**: UI micro-interactions must stay between **100ms - 200ms** (`ease_out` or critically damped springs). Motion exists to clarify spatial orientation, never to hold the user hostage.
- **Instant Keyboard / Repeat Actions**: High-frequency actions (command palette, typing, hotkey triggers) must snap instantly without transition drag. Reserve motion for state changes that have spatial continuity (toggles, popovers, drawers, chevrons).

---

## 3. The Sleek Motion & Tactile Craft Standard (Emil Kowalski & Apple Design)
- **Tactile Press Feedback**: Every clickable surface (buttons, chips, pills, selectors) MUST have physical `:active` feedback (`scale(0.97)` or tactile depth/opacity response). Nothing should ever feel numb or unresponsive under the cursor.
- **Continuous Fluid Motion**: State changes with spatial meaning (rotations, chevrons, accordions, toggles) must NEVER snap. Chevrons glide between 0° and 180° with an `ease_out` curve; toggles stretch slightly in the direction of travel (squash & stretch).
- **Physical Popover Entrances**: Menus, tooltips, and floating layers must bloom from their trigger origin with synchronized scale (`0.95 -> 1.0`) and opacity, never flat pops.
- **Liquid Segmented Controls**: Sliding selection pills must glide with inertia and smooth width interpolation across options rather than hard-cutting.
- **Concentric Geometry**: Always maintain concentric corner radii ($R_{\text{inner}} + \text{padding} = R_{\text{outer}}$) and at least 2px vertical/horizontal breathing room (`gap_0p5`) between interactive capsules so selection and hover states never collide into merged flat seams.
- **State Hierarchy**: A hover state must never overwrite or degrade a selected/active state (e.g. hovering a selected item must never dim it).
- **Interactive Defense**: Popovers and menus must debounce/suppress duplicate clicks on their trigger buttons using `menu_just_dismissed` so clicking an open trigger cleanly closes it instead of reopening.
- **Dismiss on Scroll**: Detached floating layers must immediately dismiss when the background view scrolls.

---

## 4. Pre-Completion Self-Audit
Before declaring any UI task done, capture a visual screenshot via `ui-dev.sock` (or inspect the rendered result), and verify:
1. *"Is this efficient? Does it respond immediately without sluggish delay?"*
2. *"Is this sleek and tactile? Does every click, hover, and transition feel like polished Apple/Emil-level software?"*
3. *"Did I introduce any frame pops, layout seams, or unbuffered state changes?"*
4. *"Would a world-class design engineer be proud to ship this?"*
