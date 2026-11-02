# Dotify UI/motion spec (from uiux, 2026-10-09). Surface #0F1012. No scale animations.
## 0 Tokens
Text: primary w94, secondary w60, tertiary w40, disabled w25. Green #4ADE80, amber #F5A524, red #F87171, accent #7AA7FF. Popover bg #17181B, border 1px w9, shadow 0 12 32 b45 + 0 2 6 b30.
Curves OUT(0.22,1,0.36,1) INOUT(0.65,0,0.35,1) IN(0.4,0,1,1) LINEAR. Tabular figures everywhere. Per-second tick re-renders only time text; stop ticking/breathing when window hidden, snap back on return.
## 1 Pill
h26 r13 padX10 gap6 bg w5 border w8; hover w8; pressed w10; popover open bg w10 border w14.
Idle: 14px stopwatch w55 + "Clock in" 12.5/500 w70; if Remind on & not clocked in during work hours: static 5px amber badge on icon.
Running: 6px green dot, time mono 12.5/500 w92, "·" w25, project 12.5/500 w55 truncated 120px.
Overtime: amber dot, bg amber8, border amber22, time white, tooltip "Over by 0:24".
Syncing: background sync shows nothing; manual Sync swaps dot for 10px spinner ring (w60, 270deg arc).
Offline: grey dot w35 no breathing; time keeps counting w70; tooltip "Offline · will sync"; idle: grey badge on icon.
Expired: red dot no breathing; clocked in: time w70; idle: label "Reconnect Dotify" red90; click opens popover with error banner.
Pending: spinner replaces dot/icon, label "Clocking in…"/"Clocking out…" w60, clicks ignored.
## 2 Popover
320w r12, 6px below pill, left-aligned. Section dividers 1px w6.
Header h40 pad 0 12 0 14: "Dotify" 13/600 w90; Dev env only tag "Dev" 11/500 amber on amber12 r4; right "Synced 20:13" 11.5 w40 + 24px refresh btn (14px icon w50, hover w8 r6).
Idle view: Search m12 h32 r8 bg b25 border w7, 14px icon w40, placeholder 13 w35, "/" key chip right; focus border accent55 + ring3 accent18; autofocus on open.
List max-h264 pad 4 6; section labels 11/600 uppercase ls .04em w35 pad 10 8 4: "Favorites", "Recent" (last 3, no dupes), "All projects". Rows h32 r6 pad 0 8: 8px color square (project color if API gives, else w20), name 13/450 w85 truncated, optional code 12 w40, trailing 12px star (favorites filled amber80; others on hover w30; click toggles favorite). Hover w5; keyboard highlight w7; selected bg accent14 name w95 trailing accent check. Search results flat, matched chars 600 w95. Empty states centered pad24 12.5 w45: "No projects match “xyz”" + "Clear search"; "No projects in Dotify yet" + "Refresh". Keys: ↑↓, Enter select then Enter clocks in, Cmd+D favorite, Esc clears search then closes, Tab to description. Starring doesn't move row into Favorites until next open.
Description pad12: label "What are you working on?" 11.5/500 w45, 6px gap; input like search, grows to 3 lines; prefilled w85 with "from branch" chip 10.5 w35 right, removed on first edit.
Footer pad12: left hint 12 w40 "Pick a project" -> project name w70 once chosen; right primary "Clock in ⏎" h32 pad14 r8 bg w92 label 13/600 #0F1012; disabled bg w10 label w35.
Stats strip pinned bottom bg b20 pad 10 14 12, 3 columns: label 11/500 w40; value mono 13/600 w88 + "/ 40h" 11 w35; 3px bar r2 track w8 fill w55; over target: full bar fill amber85 + "+44h" amber 11/500. Today bar only if Dotify gives a daily target (don't invent).
Clocked-in view (replaces search/list/description) pad16: breathing green dot + "Clocked in since 09:12" 11.5 w50; elapsed mono 28/600 w96 (+ amber "Overtime +0:24" chip); color square + project 14/600 w90; description 13 w60 max 2 lines; "Edit" link 12/500 w50. Clock out = secondary (bg w7 border w10 label w88) right-aligned. Confirm: first click -> "Confirm clock out" bg red16 border red35 label red95, 2px red60 line under label shrinks over 4s; second click confirms; Esc/timeout reverts.
Result: 28px circle green14 with green check, "Clocked out · 4h 15m" 14/600; project+description 12.5 w55; "09:12–13:27" mono 12 w40; buttons Edit (secondary), Done (ghost). Edit inline: project selector + description, Save (primary) Cancel (ghost). Result view stays until close; next open shows idle.
Receipt when popover closed (clock-out from right-click menu): 28px receipt pill under header pill 4s "Clocked out · 4h 15m · Edit", hover pauses.
Error banners inline below header (never toasts): m12 r8 pad 10 12, 14px icon, text 12.5 w85, action link right 12.5/600. Expired: bg red10 border red25 "Open Settings". Offline: bg w5 border w10 grey cloud-off, "You're offline. Entries will sync when you're back." + "Retry". Dotify 400: red banner, message quoted exactly up to 3 lines + "Copy".
## 3 Settings card
r12 pad16 bg w4 border w8. Header: 28px logo tile r7, "Dotify" 14/600 w92, "Clock in and out from Keel" 12.5 w50; right status chip r10 pad 2 8 11.5/500: Connected green dot on green10 text green90; Not connected w40 dot text w50 bg w6; Expired red dot red10 text red90.
Not connected: env switcher Prod/Dev h28 r7 track w6 thumb w12 border 1px w10, labels 12/500 (w90 sel / w50). Token field h34 r8 bg b25 border w7 mono 12.5 masked, inside-right show/hide eye + Paste (24px icon btns). Help line 12 w45 (where token lives). Primary "Test & connect" h32.
Connected: account row 28px initials circle (w10, 11/600), name 13/600 w90, email 12 w50, Prod/Dev tag. Buttons "Test connection" (secondary) + "Disconnect" (ghost w65, hover red + red12 bg) with same 4s two-click confirm. Divider w6, "Remind me to clock in" 13/500 w88 + subtitle 12 w45; switch 32x18 track w12 off / green80 on, 14px white knob, 2px inset. Switching env while connected asks confirmation (disconnects).
## 4 Motion
Pill idle->running: width tween 240 INOUT; old content fade out 100 + 3px up; new fades in 160 rising 3px starting at 120; dot fades in at 200, breathing starts at 600. Reverse 200ms.
Pending: label crossfade 120; spinner 900ms/turn LINEAR, only shown if wait >150ms.
Digits: change instantly. Gaining hour digit -> width tween 160 OUT. Project name change crossfade 140.
Dot breathing: opacity .45<->1, 1.8s INOUT loop; pauses when window hidden/unfocused.
Into overtime: dot green->amber + bg/border tint fade in 400 INOUT; one-time glow 4px shadow ring amber25 fading to 0 over 700ms.
Offline/expired: dot color 200; breathing eases to full opacity 200 then stops.
Popover open: opacity 0->1, translateY -6->0, 180 OUT; sections stagger 30ms (header, search, list, description, footer, stats), each 140 rising 4px. Close: opacity 0, translateY 0->-4, 120 IN, no stagger. Pill open-state bg 100ms.
View switch (idle/clocked-in/result/edit): height tween 220 INOUT; old fade out 100; new fade in 160 rising 4px starting at 80.
List: on open first 8 rows stagger 15ms, 120ms each, 3px rise; rest instant. Typing: no stagger; list crossfade 90 on results change; list height 160 OUT. Empty state fade 140. Hover 80; keyboard highlight instant.
Select project: row bg -> accent14 120; check fade 140 sliding 4px from left; footer hint crossfade 140; Clock in button disabled->enabled colors 160; star toggle crossfade 120 (moves into Favorites next open); "from branch" chip fade 120 on first edit; description grows 120 per line.
Primary button (Clock in, Save, Test & connect): fixed width while busy. Loading: label fade out 120, 14px spinner fade in (800ms/turn LINEAR). Success: spinner -> check crossfade 140, hold 400, then next state. Error: label back 120, border flash red30 600ms (in 100 out 500), error banner opens, no shake.
Clock-out confirm (also Disconnect): first click colors -> red confirm 140, label crossfade 120, width 160 OUT; 2px countdown line full->0 over 4s LINEAR; revert (timeout/Esc) colors/label/width back 160; second click -> loading state.
Result: via view switch; check circle fade 160 after 80ms delay; time range fades in 40ms after. Receipt pill: fade in + translateY -4->0 180 OUT, hold 4s (pause on hover), fade out 140 IN. Edit expands height 200 INOUT, fields fade in 30ms apart 140 each. After save, values crossfade 140.
Stats bars: fill 0->value 500 OUT, 60ms apart (Today, Week, Period), starting 200ms after open; only on first open of the day or when value changed since last open, otherwise pre-filled. Live updates tween from old 300 OUT. Overflow: fill to 100%, crossfade to amber 200, "+44h" fade in 140.
Sync spinner: refresh icon 800ms/turn LINEAR while syncing, min 400ms; on finish completes its turn and eases to stop (OUT), "Synced" time crossfade 140; on failure stops + banner opens.
Error banners: open height 0-> 180 OUT + fade 140 starting at 40; dismiss reverse 140 IN; replacement: text/icon crossfade 120, height tween only if changed.
Settings: env thumb slide 180 OUT, label colors 120; switch knob 160 OUT, track color 160. Test & connect = primary button pattern; success: not-connected section collapses height 220 INOUT fading out 100; connected section fades in rising 4px 40ms apart (account row, buttons, reminder row) 160 each; status chip text crossfade + dot/bg colors 160. Failure: red 12 error line under field opens (160 height + fade), field border -> red45 over 120 stays until edited. Test connection: same pattern; success "Connection OK" 12 green90 fade in 140, out after 3s. Disconnect: connected section collapses, form returns (mirror).
Reduced motion: no translates, width/height tweens, staggers, breathing, glow; final state immediately; bars pre-filled; spinners -> static icon at 40% (or "…"); confirm countdown -> static line + "Click again to confirm"; keep only opacity crossfades <=120ms and color changes <=120ms.
