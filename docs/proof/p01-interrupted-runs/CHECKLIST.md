# P01 visual checklist

Evaluate only committed PNGs and this checklist. Report what is visible and PASS/FAIL per applicable frame; mark non-visible technical guarantees NOT ASSESSABLE rather than infer them. No code or implementation narrative.

1. Frame01: distinguish the legacy Error card from the Interrupted card; the latter has retained-history wording and visible synthetic output, without its own failure/retry block. Other historical cards are out of scope.
2. Frame02: isolated English Interrupted status and retained output are readable; no Continue/retry/approval or active Stop control is visible for that run.
3. Frame03: Russian Прерван status and retained-history wording are readable; retained output remains accessible; no Continue/retry/approval or active Stop control is visible for that run.
4. Frame04: Activity explicitly shows no active work while interrupted history and retained output remain visible.
5. Across frames: no new clipping/overlap obscures the target status, retained-history explanation, tool arguments or output; consistent window dimensions and distinguishable states. Existing overall styling is not a P01 redesign target.
6. Images cannot prove migration atomicity, leases, database isolation, no side effects, build provenance or OS portability; do not infer those from pixels.
