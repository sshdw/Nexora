//! In-app product documentation (Docs panel): curated guides rendered from
//! verifiable sources — never hand-written prose that rots.
//!
//! Presentational over the read-only `docs_manifest` IPC wrapper (the
//! backend owns the migration version spine; the panel lists exactly the
//! versions the backend reports) plus three static registries read live:
//! the feature inventory (`DOC_FEATURES` in lib/docs.ts, one palette command
//! id per row — the id IS the feature), the SHORTCUTS registry (same grouped
//! rows as the shortcuts dialog, so combos cannot drift), and the string
//! catalog (per-version `docs.migN` notes with a bare `vN` fallback, so a
//! brand-new migration never blanks the guide before its note lands).
//!
//! Display rules: section nav (Getting started / Features / Migration /
//! Shortcuts — no FAQ: only sections with code-backed content ship), a search
//! filter over headings that hides non-matching rows and empty sections, and
//! the shared panel/tag/shortcuts primitives — zero new CSS, zero raw values.
//! The panel never animates on entry (instant render under reduced motion).

import { useEffect, useMemo, useState } from "react";

import {
  DOC_FEATURES,
  DOC_SECTIONS,
  migrationNoteKey,
  type DocsSectionId,
} from "../lib/docs";
import { SHORTCUT_GROUPS, shortcutsInGroup } from "../lib/shortcuts";
import { hasString, shortcutDesc, shortcutGroup, shortcutScope, type StringKey } from "../lib/strings";
import { docsManifest, type CommandError, type DocsManifest } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";

export interface DocsPanelProps {
  onClose: () => void;
}

function toMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).message === "string"
  ) {
    return (error as CommandError).message;
  }
  if (error instanceof Error) return error.message;
  return String(error);
}

/** Section title catalog key. */
function sectionTitle(section: DocsSectionId): StringKey {
  switch (section) {
    case "start":
      return "docs.secStart";
    case "features":
      return "docs.secFeatures";
    case "migration":
      return "docs.secMigration";
    case "shortcuts":
      return "docs.secShortcuts";
  }
}

const START_STEPS = [
  { title: "docs.start1t", body: "docs.start1b" },
  { title: "docs.start2t", body: "docs.start2b" },
  { title: "docs.start3t", body: "docs.start3b" },
] as const;

const WHATS_NEW = [
  { title: "docs.newProfilesT", body: "docs.newProfilesB" },
  { title: "docs.newRoutingT", body: "docs.newRoutingB" },
  { title: "docs.newFlagsT", body: "docs.newFlagsB" },
  { title: "docs.newKeyringT", body: "docs.newKeyringB" },
  { title: "docs.newSettingsT", body: "docs.newSettingsB" },
  { title: "docs.newRootsT", body: "docs.newRootsB" },
] as const;

export default function DocsPanel({ onClose }: DocsPanelProps) {
  const { locale, t } = useStrings();
  const [section, setSection] = useState<DocsSectionId>("start");
  const [query, setQuery] = useState("");
  const [manifest, setManifest] = useState<DocsManifest | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void docsManifest()
      .then((result) => {
        if (!cancelled) setManifest(result);
      })
      .catch((err) => {
        if (!cancelled) {
          setManifest(null);
          setError(toMessage(err));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const needle = query.trim().toLowerCase();
  const matches = (text: string) =>
    needle === "" || text.toLowerCase().includes(needle);

  const visibleFeatures = useMemo(
    () =>
      DOC_FEATURES.filter((feature) =>
        matches(
          `${t(feature.titleKey)} ${t(feature.bodyKey)} ${feature.paletteId}`,
        ),
      ),
    [needle, t],
  );

  const visibleMigrations = useMemo(() => {
    const rows = manifest?.migrations ?? [];
    return rows.filter((entry) => {
      const key = migrationNoteKey(entry.version);
      const note = hasString(key) ? t(key) : `v${entry.version}`;
      return matches(`v${entry.version} ${note}`);
    });
  }, [needle, t, manifest]);

  const visibleWhatsNew = useMemo(
    () =>
      WHATS_NEW.filter((item) => matches(`${t(item.title)} ${t(item.body)}`)),
    [needle, t],
  );

  const visibleStarts = useMemo(
    () => START_STEPS.filter((step) => matches(`${t(step.title)} ${t(step.body)}`)),
    [needle, t],
  );

  const visibleShortcutGroups = useMemo(
    () =>
      SHORTCUT_GROUPS.map((group) => ({
        group,
        rows: shortcutsInGroup(group).filter((entry) =>
          matches(
            `${entry.keys.join(" ")} ${shortcutDesc(locale, entry.id, entry.description)} ${entry.scope}`,
          ),
        ),
      })).filter((grouped) => grouped.rows.length > 0),
    [needle, locale],
  );

  // With an active filter, sections without a hit drop out (except while the
  // migration spine is still loading — the rows do not exist yet).
  const filtering = needle !== "";
  const showStart = !filtering || visibleStarts.length > 0;
  const showFeatures = !filtering || visibleFeatures.length > 0;
  const showMigration =
    !filtering ||
    loading ||
    visibleWhatsNew.length > 0 ||
    visibleMigrations.length > 0;
  const showShortcuts = !filtering || visibleShortcutGroups.length > 0;
  const nothingMatches =
    filtering &&
    !loading &&
    visibleStarts.length === 0 &&
    visibleFeatures.length === 0 &&
    visibleWhatsNew.length === 0 &&
    visibleMigrations.length === 0 &&
    visibleShortcutGroups.length === 0;

  const showSection = (id: DocsSectionId): boolean => {
    switch (id) {
      case "start":
        return showStart;
      case "features":
        return showFeatures;
      case "migration":
        return showMigration;
      case "shortcuts":
        return showShortcuts;
    }
  };

  return (
    <div className="nex-vcs" role="group" aria-label={t("docs.group")}>
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">{t("docs.title")}</h2>
          <p className="nex-vcs-subtitle">{t("docs.subtitle")}</p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button variant="quiet" onClick={onClose}>
            {t("common.backToConversations")}
          </M3Button>
        </div>
      </header>

      <div className="nex-vcs-body">
        <label className="nex-vcs-notice" htmlFor="nex-docs-search">
          {t("docs.searchLabel")}
        </label>
        <input
          id="nex-docs-search"
          className="nex-input"
          type="search"
          value={query}
          placeholder={t("docs.searchPh")}
          onChange={(event) => setQuery(event.target.value)}
        />
        <nav aria-label={t("docs.group")}>
          {DOC_SECTIONS.filter(showSection).map((id) => (
            <M3Button
              key={id}
              variant="quiet"
              onClick={() => setSection(id)}
              aria-current={section === id ? "true" : undefined}
            >
              {t(sectionTitle(id))}
            </M3Button>
          ))}
        </nav>

        {nothingMatches ? (
          <p className="nex-agent-empty">{t("docs.noMatch")}</p>
        ) : (
          <>
            {(section === "start" || filtering) && showStart && (
              <section className="nex-vcs-section" aria-label={t("docs.secStart")}>
                {!filtering && (
                  <h3 className="nex-vcs-section-title">{t("docs.secStart")}</h3>
                )}
                <p className="nex-vcs-notice">{t("docs.startIntro")}</p>
                <ol>
                  {(filtering ? visibleStarts : [...START_STEPS]).map((step) => (
                    <li key={step.title}>
                      <p className="nex-vcs-notice">{t(step.title)}</p>
                      <p className="nex-vcs-notice">{t(step.body)}</p>
                    </li>
                  ))}
                </ol>
              </section>
            )}

            {(section === "features" || filtering) && showFeatures && (
              <section
                className="nex-vcs-section"
                aria-label={t("docs.secFeatures")}
              >
                {!filtering && (
                  <h3 className="nex-vcs-section-title">
                    {t("docs.secFeatures")}
                  </h3>
                )}
                <ul className="nex-vcs-file-list">
                  {(filtering ? visibleFeatures : [...DOC_FEATURES]).map(
                    (feature) => (
                      <li key={feature.paletteId} className="nex-vcs-file-row">
                        <p className="nex-vcs-notice">{t(feature.titleKey)}</p>
                        <p className="nex-vcs-notice">{t(feature.bodyKey)}</p>
                        <span className="nex-tag nex-tag-mono">
                          {feature.paletteId}
                        </span>
                      </li>
                    ),
                  )}
                </ul>
              </section>
            )}

            {(section === "migration" || filtering) && showMigration && (
              <section
                className="nex-vcs-section"
                aria-label={t("docs.secMigration")}
              >
                {!filtering && (
                  <h3 className="nex-vcs-section-title">
                    {t("docs.secMigration")}
                  </h3>
                )}
                <p className="nex-vcs-notice">{t("docs.migIntro")}</p>
                <h4 className="nex-vcs-section-title">{t("docs.newTitle")}</h4>
                <ul className="nex-vcs-file-list">
                  {(filtering ? visibleWhatsNew : [...WHATS_NEW]).map((item) => (
                    <li key={item.title} className="nex-vcs-file-row">
                      <p className="nex-vcs-notice">{t(item.title)}</p>
                      <p className="nex-vcs-notice">{t(item.body)}</p>
                    </li>
                  ))}
                </ul>
                {loading && <M3LoadingIndicator label={t("common.loading")} />}
                {error && (
                  <div className="nex-composer-error nex-fade-in" role="alert">
                    {error}
                  </div>
                )}
                {!loading && !error && manifest && (
                  <>
                    <p className="nex-vcs-notice">
                      {t("docs.migSchema", { n: manifest.schema_version })}
                    </p>
                    <ul className="nex-vcs-file-list">
                      {(filtering ? visibleMigrations : manifest.migrations).map(
                        (entry) => {
                          const key = migrationNoteKey(entry.version);
                          const note = hasString(key)
                            ? t(key)
                            : `v${entry.version}`;
                          return (
                            <li
                              key={entry.version}
                              className="nex-vcs-file-row"
                            >
                              <span className="nex-tag nex-tag-mono">
                                v{entry.version}
                              </span>{" "}
                              <span className="nex-vcs-notice">{note}</span>
                            </li>
                          );
                        },
                      )}
                    </ul>
                  </>
                )}
              </section>
            )}

            {(section === "shortcuts" || filtering) && showShortcuts && (
              <section
                className="nex-vcs-section"
                aria-label={t("docs.secShortcuts")}
              >
                {!filtering && (
                  <h3 className="nex-vcs-section-title">
                    {t("docs.secShortcuts")}
                  </h3>
                )}
                <div className="nex-shortcuts">
                  <ul className="nex-shortcuts-groups">
                    {(filtering
                      ? visibleShortcutGroups
                      : SHORTCUT_GROUPS.map((group) => ({
                          group,
                          rows: shortcutsInGroup(group),
                        }))
                    ).map(({ group, rows }) => (
                      <li key={group} className="nex-shortcuts-group">
                        <h4 className="nex-shortcuts-heading">
                          {shortcutGroup(locale, group)}
                        </h4>
                        <ul className="nex-shortcuts-rows">
                          {rows.map((entry) => {
                            const description = shortcutDesc(
                              locale,
                              entry.id,
                              entry.description,
                            );
                            const scope = shortcutScope(locale, entry.scope);
                            return (
                              <li
                                key={entry.id}
                                className="nex-shortcuts-row"
                                aria-label={`${entry.keys.join(", ")}: ${description} (${scope})`}
                              >
                                <span
                                  className="nex-shortcuts-keys"
                                  aria-hidden="true"
                                >
                                  {entry.keys.map((label) => (
                                    <kbd key={label}>{label}</kbd>
                                  ))}
                                </span>
                                <span className="nex-shortcuts-text">
                                  <span className="nex-shortcuts-desc">
                                    {description}
                                  </span>
                                  <span className="nex-shortcuts-scope">
                                    {scope}
                                  </span>
                                </span>
                              </li>
                            );
                          })}
                        </ul>
                      </li>
                    ))}
                  </ul>
                </div>
              </section>
            )}
          </>
        )}
      </div>
    </div>
  );
}
