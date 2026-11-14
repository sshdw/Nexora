# Stage5 PR5A/P35 — native visual and interaction checklist

Review only committed real-build frames and their index, not product code or implementer reasoning. Owner excludes nexora-visual-design/material-3-expressive skills; evaluate against existing monochrome Nexora authority and ui-ux-pro-max observable usability. This is an existing desktop UI refinement, not a new visual system.

Expected frame/journey coverage:
- Baseline16404907: empty shell, Sessions actions, one session/header, multiple tabs/workspace duplication, two split headers.
- Final empty/one/multiple session: Split occupies one stable top-strip trailing position; unavailable state visibly disabled, sufficient session identity without duplicate header/workspace chip.
- Final Split: exactly one Split/Close split control, two quiet compact pane title badges, long-title truncation/full title access, independent pane content/drafts and archived status identification where pictured.
- Sessions: Export alongside existing row actions, row identity readable at normal/narrow width, actions keyboard-reachable and focus visible. Non-active and archived Export routes name the selected row, not the current conversation.
- Palette: existing Export command reachable through query/keyboard; correct target modal. Export file receipts validate actual payload separately; do not infer file IO success from a dialog alone.
- Final narrow width and EN/RU labels where actually captured: no control clipping/overlap, stable Split placement, readable labels and focus/disabled state.

For every frame give one-line what-I-see and per-applicable-item PASS/FIX/unassessed. Check the13 existing design-system questions: two-second readability; visible agent state where present; discoverable change; stable controls; recognizably Nexora; restrained/no decorative accent; necessary borders/cards; meaningful motion if observable; usability without animation if evidenced; denser/clearer; no unnecessary constant-attention element; information uses existing surfaces; grayscale/keyboard clarity. Single still frames do not prove motion or runtime execution. The purple cursor marker is an automation/OS pointer effect, not the app theme.

Do not mark unpictured streaming/provider errors/loading, OS reduced-motion, approval execution or security controls as visually verified. PR5B owns new run/composer presentation; no provider/keyring request may be fabricated for this PR5A proof. Functional export IDs, split lifecycle and data parity rely on actual interaction/service/component/file receipts plus independent code review. Flag genuine visible regressions; do not request optional restyling unrelated to P35.

Frames are raw full-window actual Tauri captures, unique md5 and >900 colors; geometry is fixed for standard before/after comparisons, with explicitly named narrow-window cases. No cropping, mock-only proof or made-up provider states. Owner taste verdict on committed frames is required before Stage5 merge.
