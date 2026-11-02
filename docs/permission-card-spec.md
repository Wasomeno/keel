(uiux spec — see conversation; summarized faithfully)
CARD: inline last item of active turn, reading column width (<=760). radius 12, padding 16 sides/14 top/12 bottom. bg white 4%, border 1px white 9%, 1px top inner highlight white 5%, shadow 0 8 24 black 35%. No amber outline.
Row1 status (h16): 6px amber dot #F5A524 (breathes opacity .45->1, 1.8s loop) + "Needs approval" 12/500 amber 90%; right: source tag e.g. "keel-browser" mono 11/500 white 40%.
Row2 title (gap 8): 24px icon tile r6 bg white 6% glyph 14px white 75%; gap 10; humanized title 15/600 white 94% from lookup table (navigate->"Navigate browser", write_file->"Edit file", run_command->"Run command"; fallback split _ sentence case).
Row3 optional description (gap 10): 13/400 white 60% e.g. "Open a page in the agent's browser".
Row4 args (gap 10): block r8 pad 10x12 bg black 25% border 1px white 5%; lines key mono 11.5 white 40%, gap 12, value mono 12.5 lh20 white 85%; primary arg (url/command/path) first; wrap, max 3 lines then "Show all" 12/500 white 55%; commands prefixed `$` white 35%.
Row5 actions (gap 14): one row [Allow 1][Allow always 2][Deny 3] left, gap 8, spacer, Stop run right.
Keys: 1/2/3 left->right; Enter=Allow, Cmd+Enter=Allow always, Esc=Deny; Tab/arrows move focus. (Changes mapping: Allow always was 1.)
Button: h32 padX12 r8 label 13/500 + trailing key chip 18x18 r4 mono 11/500 ml6 (secondary/ghost: bg white8% text white50%; primary: bg black10% text black55%).
Allow primary: bg white 92%, label #0F1012 90%; hover white100; pressed white85 scale .98.
Allow always secondary: bg white7% border 1px white10% label white85; hover bg 11%; pressed 14% scale .98.
Deny ghost: no bg label white65; hover bg red #F87171 12% label red 95%; pressed red 18% scale .98.
Stop run: h28 pad10 12/500 white40 + 10px square stop icon; hover red label + bg red10%; Cmd+. in tooltip.
Focus: 1.5px accent #7AA7FF 80% border + outer ring 0 0 0 3 accent 22%; only after keyboard use; initial focus Allow without ring.
Timing: color 80ms, press scale 60ms, release 120ms.
After answer: chosen button holds pressed ~100ms, check icon replaces key hint, then exit.
EASING: out-quint (0.22,1,0.36,1); in-out (0.65,0,0.35,1).
ENTRY ~300ms: card opacity 0->1, translateY 8->0, scale .985->1, 220ms ease-out. Contents fade+rise 4px 160ms staggered: status+title 0ms, args 40ms, buttons 80/110/140ms. If transcript follows bottom, snap-scroll card into view.
EXIT after answer ~380ms: 0-100 check on chosen; 100-220 others fade to 0 (other buttons, args, status); 120-320 height eases to 28px receipt (in-out), bg/border/shadow fade 0, radius ->6; 220-380 receipt content fade in with 2px rise. Receipt styled like existing tool rows: Allow "✓ Allowed · Navigate browser · example.com" (check green 80%, text 12.5/500 white55, arg mono white40); Allow always "✓ Always allowed · Navigate browser" + "Manage" link on hover; Deny "✕ Denied · Navigate browser" ✕ red 70%. Clicking receipt expands to show original args read-only.
EXIT on Stop/cancel/timeout ~260ms: fade + translateY 0->4 over 120ms, then height collapse to receipt 160ms; receipt "■ Run stopped" / "Request canceled" white 45%.
REDUCED MOTION: no translate/scale/stagger/height anim/dot breathing; entry 120ms opacity fade; exit instant swap w/ 120ms crossfade.
OFFSCREEN: if card scrolled out of view, amber "Needs approval" chip above composer; click scrolls to card; number shortcuts work from composer.
QUESTION VARIANT: status "Question" accent blue 90% + blue dot; icon tile "?"; title = question 15/600 up to 4 lines + optional context 13 white60. Options vertical list gap 6; row min-h40 pad 10x12 r8 bg white3% border 1px white6%; key chip 1-9, gap 10, label 13.5/500 white88, optional 2nd line 12/400 white50; hover bg 6%; focus border accent 55% + bg 6%; pressed scale .99; "Recommended" tag 11/500 accent 80% right. Single-select answers on pick. Multi-select: 16px checkbox r4 instead of number, number keys toggle, footer primary "Submit ⏎" disabled 40% until >=1 checked. Free text: last row "Other…" key O expands 160ms into 36px input 13.5; Enter sends, Esc collapses. Footer hint 11 white35 "↑↓ move · ⏎ choose · Esc skip" + Stop run right. Rows stagger 30ms (cap 6). Receipt "Answered · <label>" / "Skipped question".
Colors assume surface ~#0F1012; map to existing theme tokens.
