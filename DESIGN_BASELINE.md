# DESIGN_BASELINE — ptask Triage Cockpit

Written before any change (2026-07-06). Every design judgment in this pass cites
this file, not trends. Scope: the web dashboard (`dashboard/www/index.html`,
served by `pt serve`). The TUI and CLI have their own idioms and are out of
scope for this pass.

## 1. Product intent (3 lines)

- **Who:** one operator (and HAL's agents) running PureTensor's task queue — an
  expert user who visits many times a day, often for under a minute.
- **For what:** *triage*, not project management — see what's critical, burn it
  down (done/dismiss/snooze/re-prioritize), capture new tasks at zero friction.
- **Character:** a **professional telemetry instrument**. Dense, calm, precise,
  dark-first, keyboard-first. An ops console, not an editorial page and not a
  toy — with one sanctioned exception (see §2, LCARS).

## 2. Existing design artifacts, read as hypotheses

There is no standalone design doc; the de facto system is the CSS token block
in `index.html` plus the theme table in `dashboard/README.md`.

- **Four runtime themes** (Mission Control / Crystal / LCARS / Exec) are a
  deliberate, operator-loved feature. LCARS is *sanctioned playfulness* — a
  costume over the same bones. Verdict: keep all four; the costume must never
  cost correctness (contrast, overflow, touch targets) on any width.
- **Token block hypothesis:** "every visual token is a CSS custom property."
  Partially true — radii (14/10/9/8/7/6/5px), font sizes (9–18px, eight steps),
  and hardcoded rgba values leak outside the token system. Contradiction with
  the stated intent; candidates for consolidation.
- **Glow/gradient/glassmorphism** are earned by the Mission/Crystal identities
  (telemetry glow, crystal-cube brand). Verdict: keep, but they must not carry
  meaning alone and must not break contrast floors.
- **Emoji as control glyphs** (💤 🗑 ✓ 🎤 on buttons): *not* earned by a
  telemetry instrument — they render inconsistently across platforms, clash
  hardest in LCARS/Exec, and are invisible to assistive tech unless labeled.
  Hypothesis to test in audit: replace control emoji with labeled glyphs/SVG;
  emoji in toasts (transient, informal) may stay.
- **Density** (10–12px meta text) is intent-consistent for a pro instrument.
  Density is not a license for sub-AA contrast; where the two fight, contrast
  wins and size holds the floor at ~10px.

## 3. What this product should feel like at its best

Glanceable in three seconds: one unmistakable focal point (Critical Now),
priority color doing the ranking work, everything else quiet. Every action is
two interactions or fewer and always confirms itself (toast) without moving
the operator's reading position. Keyboard path is first-class and *visible*
(focus rings, kbd hints); mouse and phone paths are never second-class —
what works at 1440px works at 390px, in all four themes. The board never
lies and never dead-ends: loading, empty, and error are all designed states.
Motion is functional (arrival flash, live pulse) and fully collapses under
reduced-motion.

## 4. Core user paths (audit walks exactly these)

Single-route SPA — paths are flows, each walked at 1440px, ~830px, 390px
(+ a 320px reflow spot-check), in Mission (default) with per-theme sweeps:

- **P1 Triage sweep:** load → scan chips + Critical Now → mark done (confirm
  dialog) → toast → board updates.
- **P2 Capture:** quick-add seed → composer modal (title/desc/severity/deadline,
  voice states incl. the no-HTTPS fallback) → create → toast.
- **P3 Re-prioritize:** priority chip → popover menu (mouse + arrows/Escape) →
  pick level.
- **P4 Keyboard triage:** j/k selection, x/d/s/e/o/1-5, r review mode round-trip.
- **P5 Investigate:** title → detail drawer (description + history) → close.
- **P6 Deadline scan:** timeline read (label collisions, point affordance).
- **P7 Neglect scan:** heatmap read (contrast of counts on heat colors).
- **P8 Theme switch:** all four themes at all three widths.
- **P9 Degraded states:** first-load (loading), backend down (error), empty DB
  (empty states), tab/zoom-200% pass over P1–P5.
