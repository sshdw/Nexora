//! GitHub Issues & PRs panel: read-only lists for the workspace origin repo.
//!
//! Presentational over the two `gh_issues` / `gh_pulls` IPC wrappers: one
//! capped page (at most 50 items) per kind tab + state filter (`open` /
//! `closed` / `all`), with list + detail views. Loads on mount and on every
//! kind/state change (reads are cheap GETs), plus manual Refresh — there is
//! no watching or live polling. Read-only end to end: no commenting,
//! labeling, or merging exists anywhere on this path.
//!
//! Display rules (secret-free): states, labels, and counts render as
//! fixed-vocabulary catalog tags (unknown tokens echo defensively); rows
//! show `#number · author` plus the capped backend body (at most 8000 chars
//! with a truncation notice). No-token responses keep their lists and add
//! the connect-hint banner (never an error dump); quota exhaustion renders
//! the rate-limit state with the remaining/limit snapshot (never a silent
//! empty); a non-github.com origin renders the dedicated no-remote state.
//! The panel never animates on entry (instant render under reduced motion);
//! all visuals ride the shared panel/tag/notice primitives — zero new CSS,
//! zero raw values.

import { useCallback, useEffect, useState, type ReactNode } from "react";

import {
  ghIssues,
  ghPulls,
  type CommandError,
  type GhIssue,
  type GhPull,
  type GhRateLimit,
  type GhState,
} from "../lib/tauri";
import { useStrings, type Strings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";
import M3SegmentedGroup from "./M3SegmentedGroup";

export interface IssuesPanelProps {
  onClose: () => void;
}

type Kind = "issues" | "pulls";

function toMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).message === "string"
  ) {
    return (error as CommandError).message;
  }
  return "Issues & PRs are unavailable.";
}

/** Backend state vocabulary echoed defensively (the API only ever sends
 * `open`/`closed`; anything else renders the catalog unknown tag). */
function stateTag(state: string, t: Strings["t"]): string {
  if (state === "open") return t("gh.tagOpen");
  if (state === "closed") return t("gh.tagClosed");
  return t("gh.tagUnknown");
}

/** Short calendar date from an ISO timestamp (`YYYY-MM-DD`); the raw text
 * is echoed only when the timestamp does not parse. */
function shortDate(iso: string | null): string | null {
  if (iso === null || iso === "") return null;
  const time = Date.parse(iso);
  if (Number.isNaN(time)) return iso;
  return new Date(time).toISOString().slice(0, 10);
}

function RateNote({ rate }: { rate: GhRateLimit | null }): ReactNode {
  const { t } = useStrings();
  if (rate === null) return null;
  if (rate.limit == null || rate.remaining == null) {
    return (
      <p className="nex-vcs-notice" role="note">
        {t("gh.rateUnknown")}
      </p>
    );
  }
  return (
    <p className="nex-vcs-notice" role="note">
      {t("gh.rateNote", { remaining: rate.remaining, limit: rate.limit })}
    </p>
  );
}

export default function IssuesPanel({ onClose }: IssuesPanelProps) {
  const { t } = useStrings();
  const [kind, setKind] = useState<Kind>("issues");
  const [state, setState] = useState<GhState>("open");
  const [issues, setIssues] = useState<GhIssue[]>([]);
  const [pulls, setPulls] = useState<GhPull[]>([]);
  const [owner, setOwner] = useState<string | null>(null);
  const [repo, setRepo] = useState<string | null>(null);
  const [authenticated, setAuthenticated] = useState(true);
  const [rateLimited, setRateLimited] = useState(false);
  const [rate, setRate] = useState<GhRateLimit | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (loading) return;
    setLoading(true);
    setError(null);
    try {
      if (kind === "issues") {
        const result = await ghIssues(state);
        setIssues(result.items);
        setOwner(result.owner);
        setRepo(result.repo);
        setAuthenticated(result.authenticated);
        setRateLimited(result.rate_limited);
        setRate(result.rate_limit);
      } else {
        const result = await ghPulls(state);
        setPulls(result.items);
        setOwner(result.owner);
        setRepo(result.repo);
        setAuthenticated(result.authenticated);
        setRateLimited(result.rate_limited);
        setRate(result.rate_limit);
      }
      setSelected(null);
    } catch (err) {
      setError(toMessage(err));
    } finally {
      setLoading(false);
    }
  }, [kind, state]);

  useEffect(() => {
    void load();
  }, [load]);

  const noRemote = error !== null && error.includes("github.com");
  const items: Array<{ number: number }> =
    kind === "issues" ? issues : pulls;
  const selectedIssue =
    kind === "issues"
      ? issues.find((item) => item.number === selected) ?? null
      : null;
  const selectedPull =
    kind === "pulls"
      ? pulls.find((item) => item.number === selected) ?? null
      : null;
  const detail: GhIssue | GhPull | null = selectedIssue ?? selectedPull;
  const subtitle =
    owner !== null && repo !== null
      ? `${t("gh.subtitle")} ${t("gh.repo", { owner, repo })}`
      : t("gh.subtitle");

  return (
    <div className="nex-vcs" role="group" aria-label={t("gh.group")}>
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">{t("gh.title")}</h2>
          <p className="nex-vcs-subtitle">{subtitle}</p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button variant="quiet" onClick={() => void load()} disabled={loading}>
            {loading ? t("gh.refreshing") : t("gh.refresh")}
          </M3Button>
          <M3Button variant="quiet" onClick={onClose}>
            {t("common.backToConversations")}
          </M3Button>
        </div>
      </header>

      <div className="nex-vcs-body">
        <M3SegmentedGroup<Kind>
          label={t("gh.kindLabel")}
          semantics="tabs"
          options={[
            { value: "issues", label: t("gh.kindIssues") },
            { value: "pulls", label: t("gh.kindPulls") },
          ]}
          value={kind}
          onChange={(next) => {
            setKind(next);
            setSelected(null);
          }}
        />
        <M3SegmentedGroup<GhState>
          label={t("gh.stateLabel")}
          options={[
            { value: "open", label: t("gh.stateOpen") },
            { value: "closed", label: t("gh.stateClosed") },
            { value: "all", label: t("gh.stateAll") },
          ]}
          value={state}
          onChange={(next) => {
            setState(next);
            setSelected(null);
          }}
        />

        {!authenticated && error === null && (
          <p className="nex-vcs-notice" role="note">
            {t("gh.noToken")}
          </p>
        )}
        <RateNote rate={rate} />

        {loading && items.length === 0 && (
          <M3LoadingIndicator label={t("gh.loading")} />
        )}
        {noRemote ? (
          <p className="nex-agent-empty" role="note">
            {t("gh.noRemote")}
          </p>
        ) : error !== null ? (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {error}
          </div>
        ) : rateLimited ? (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {t("gh.rateLimited")}
          </div>
        ) : items.length === 0 && !loading ? (
          <p className="nex-agent-empty">
            {kind === "issues" ? t("gh.emptyIssues") : t("gh.emptyPulls")}
          </p>
        ) : (
          <>
            <ul className="nex-vcs-file-list" aria-label={t("gh.listAria")}>
              {(kind === "issues" ? issues : pulls).map((item) => (
                <li key={item.number} className="nex-vcs-file-row">
                  <M3Button
                    variant="quiet"
                    onClick={() =>
                      setSelected((current) =>
                        current === item.number ? null : item.number,
                      )
                    }
                    aria-expanded={selected === item.number}
                  >
                    #{item.number} · {item.title}
                  </M3Button>{" "}
                  <span className="nex-tag nex-tag-mono">
                    {stateTag(item.state, t)}
                  </span>{" "}
                  <span className="nex-tag nex-tag-mono">
                    {t("gh.byLine", { n: item.number, author: item.author })}
                  </span>
                </li>
              ))}
            </ul>
            {detail === null ? (
              <p className="nex-vcs-notice" role="note">
                {t("gh.selectHint")}
              </p>
            ) : (
              <section
                className="nex-vcs-section"
                aria-label={t("gh.detailAria")}
              >
                <h3 className="nex-vcs-section-title">
                  #{detail.number} · {detail.title}
                </h3>
                <p className="nex-vcs-notice">
                  <span className="nex-tag nex-tag-mono">
                    {stateTag(detail.state, t)}
                  </span>{" "}
                  {kind === "pulls" && (detail as GhPull).draft && (
                    <>
                      <span className="nex-tag nex-tag-mono">
                        {t("gh.tagDraft")}
                      </span>{" "}
                    </>
                  )}
                  {t("gh.byLine", { n: detail.number, author: detail.author })}
                  {shortDate(detail.created_at) !== null &&
                    ` · ${shortDate(detail.created_at)}`}
                  {" · "}
                  {t("gh.comments", { n: detail.comments })}
                </p>
                {kind === "pulls" &&
                  (detail as GhPull).head_ref !== null &&
                  (detail as GhPull).base_ref !== null && (
                    <p className="nex-vcs-notice">
                      <span className="nex-tag nex-tag-mono">
                        {t("gh.refsLine", {
                          head: (detail as GhPull).head_ref ?? "",
                          base: (detail as GhPull).base_ref ?? "",
                        })}
                      </span>
                    </p>
                  )}
                {kind === "issues" &&
                  (detail as GhIssue).labels.length > 0 && (
                    <p className="nex-vcs-notice">
                      {t("gh.labelsTitle")}:{" "}
                      {(detail as GhIssue).labels.map((label) => (
                        <span
                          key={label}
                          className="nex-tag nex-tag-mono"
                          title={label}
                        >
                          {label}
                        </span>
                      ))}
                    </p>
                  )}
                {detail.body_truncated && (
                  <p className="nex-vcs-notice" role="note">
                    {t("gh.bodyTruncated")}
                  </p>
                )}
                <pre className="nex-agent-terminal-stdout">
                  {detail.body === "" ? t("gh.noBody") : detail.body}
                </pre>
              </section>
            )}
          </>
        )}
      </div>
    </div>
  );
}
