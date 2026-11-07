//! Tab vocabulary shared by the Activity & Health panel and its two tab
//! bodies (AC-208 split). Defined here so the tab components can type their
//! props without importing from the parent file (which imports them).

export type ActivityHealthTab = "activity" | "health";
export type ActivityKind = "all" | "runs" | "spend" | "conversations";
