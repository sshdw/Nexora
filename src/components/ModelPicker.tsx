//! Model picker (R6-02) + connection grouping & Effort slider (M06 / P10, P11, P29).
//!
//! Behaviour:
//!   - the search field filters model ids (and connection names) case-insensitively;
//!   - ArrowUp/Down/Home/End move the active option (the input keeps
//!     focus and `aria-activedescendant` names the row);
//!   - Enter picks the active row, Escape closes;
//!   - clicking a row picks it and closes;
//!   - when capability metadata is provided, displays ONLY the effort levels
//!     supported by the active `(connection_id, model_id)` (hidden/disabled with
//!     an honest indicator when unsupported, unless manually overridden).

import { Fragment, useEffect, useId, useMemo, useRef, useState } from "react";

import { useComposerPopoverLayout } from "../lib/composerPopover";
import type { ConnectionModelCapabilities } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import { Button } from "./ui/button";
import {
  Popover,
  PopoverContent,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
} from "./ui/popover";
import { Slider } from "./ui/slider";
import { CheckIcon, ChevronDownIcon, SearchIcon } from "./icons";

export interface ModelConnectionGroup {
  connectionId: string;
  connectionName: string;
  models: readonly string[];
}

export interface ModelPickerProps {
  /** Selected model, or null when none is configured. */
  selectedModel: string | null;
  /** Models offered by the selected provider. */
  models: readonly string[];
  /** Persist a model choice. The provider store owns the result. */
  onSelectModel: (model: string) => void;
  /** Optional connection-grouped model catalog. */
  connectionGroups?: readonly ModelConnectionGroup[];
  /** Active connection ID when multiple connections are shown. */
  selectedConnectionId?: string | null;
  /** Persist both connection & model when choosing from a connection group. */
  onSelectConnectionModel?: (connectionId: string, model: string) => void;
  /** Capabilities for the currently selected `(connection_id, model_id)`. */
  capabilities?: ConnectionModelCapabilities | null;
  /** Capabilities lookup keyed by `${connectionId}::${modelId}`. */
  capabilitiesByKey?: ReadonlyMap<string, ConnectionModelCapabilities>;
  /** Selected reasoning effort level (`low`, `medium`, `high`, `xhigh`). */
  selectedEffort?: string | null;
  /** Persist or clear the reasoning effort level. */
  onSelectEffort?: (effort: string | null) => void;
  /** Last model-selection failure, surfaced inside the popover. */
  modelError?: string | null;
  /** Deep link to the provider section of Settings. */
  onOpenSettings: () => void;
}

interface PickerEntry {
  key: string;
  model: string;
  connectionId: string | null;
  connectionName: string | null;
}

export default function ModelPicker({
  selectedModel,
  models,
  onSelectModel,
  connectionGroups,
  selectedConnectionId = null,
  onSelectConnectionModel,
  capabilities = null,
  capabilitiesByKey,
  selectedEffort = null,
  onSelectEffort,
  modelError = null,
  onOpenSettings,
}: ModelPickerProps) {
  const { t } = useStrings();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const layout = useComposerPopoverLayout();
  const listId = useId();
  const optionId = (index: number): string => `${listId}-option-${index}`;

  const groupedMode =
    connectionGroups !== undefined && connectionGroups.length > 0;

  const entries = useMemo<PickerEntry[]>(() => {
    if (groupedMode && connectionGroups) {
      const out: PickerEntry[] = [];
      for (const group of connectionGroups) {
        for (const model of group.models) {
          out.push({
            key: `${group.connectionId}::${model}`,
            model,
            connectionId: group.connectionId,
            connectionName: group.connectionName,
          });
        }
      }
      return out;
    }
    return models.map((model) => ({
      key: model,
      model,
      connectionId: selectedConnectionId,
      connectionName: null,
    }));
  }, [groupedMode, connectionGroups, models, selectedConnectionId]);

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (needle === "") return entries;
    return entries.filter(
      (entry) =>
        entry.model.toLowerCase().includes(needle) ||
        (entry.connectionName !== null &&
          entry.connectionName.toLowerCase().includes(needle))
    );
  }, [entries, query]);

  const activeCapabilities = useMemo(() => {
    if (capabilities !== null) return capabilities;
    if (selectedConnectionId && selectedModel && capabilitiesByKey) {
      return (
        capabilitiesByKey.get(`${selectedConnectionId}::${selectedModel}`) ??
        null
      );
    }
    return null;
  }, [capabilities, capabilitiesByKey, selectedConnectionId, selectedModel]);

  const supportedEfforts = useMemo(
    () => activeCapabilities?.supported_efforts ?? [],
    [activeCapabilities]
  );

  const effectiveEffort = useMemo(() => {
    if (supportedEfforts.length === 0) return null;
    if (selectedEffort && supportedEfforts.includes(selectedEffort)) {
      return selectedEffort;
    }
    if (supportedEfforts.includes("medium")) return "medium";
    return supportedEfforts[0] ?? null;
  }, [supportedEfforts, selectedEffort]);

  // Opening (or changing the query) puts the active row on the current
  // selection when it survives the filter, otherwise on the first row.
  useEffect(() => {
    if (!open) return;
    const selected = filtered.findIndex(
      (entry) =>
        entry.model === selectedModel &&
        (!groupedMode ||
          !selectedConnectionId ||
          entry.connectionId === selectedConnectionId)
    );
    setActive(selected >= 0 ? selected : 0);
  }, [open, filtered, selectedModel, groupedMode, selectedConnectionId]);

  const handleOpenChange = (next: boolean) => {
    // Measure before the content mounts, so the first paint already has the
    // card-relative offset and height cap (R6-02 defect fix).
    if (next) layout.measure();
    setOpen(next);
    if (!next) setQuery("");
  };

  const pick = (entry: PickerEntry) => {
    if (entry.connectionId && onSelectConnectionModel) {
      onSelectConnectionModel(entry.connectionId, entry.model);
    } else {
      onSelectModel(entry.model);
    }
    setOpen(false);
    setQuery("");
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (filtered.length === 0) {
      if (event.key === "Escape") {
        event.preventDefault();
        setOpen(false);
        setQuery("");
      }
      return;
    }
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        setActive((index) => (index + 1) % filtered.length);
        break;
      case "ArrowUp":
        event.preventDefault();
        setActive((index) => (index - 1 + filtered.length) % filtered.length);
        break;
      case "Home":
        event.preventDefault();
        setActive(0);
        break;
      case "End":
        event.preventDefault();
        setActive(filtered.length - 1);
        break;
      case "Enter": {
        event.preventDefault();
        const entry = filtered[active];
        if (entry !== undefined) pick(entry);
        break;
      }
      case "Escape":
        event.preventDefault();
        setOpen(false);
        setQuery("");
        break;
    }
  };

  const noModels = entries.length === 0;
  const noMatch = !noModels && filtered.length === 0;

  const formatEffortLabel = (level: string): string => {
    switch (level) {
      case "low":
        return t("conv.effortLevel.low");
      case "medium":
        return t("conv.effortLevel.medium");
      case "high":
        return t("conv.effortLevel.high");
      case "xhigh":
        return t("conv.effortLevel.xhigh");
      default:
        return level;
    }
  };

  const formatProvenance = (
    provenance: ConnectionModelCapabilities["provenance"]
  ): string => {
    switch (provenance) {
      case "manual_override":
        return t("conv.provenance.manual_override");
      case "api_metadata":
        return t("conv.provenance.api_metadata");
      case "built_in_catalog":
        return t("conv.provenance.built_in_catalog");
      default:
        return t("conv.provenance.unknown");
    }
  };

  const currentEffortIndex =
    effectiveEffort !== null ? supportedEfforts.indexOf(effectiveEffort) : -1;

  return (
    <Popover open={open} onOpenChange={handleOpenChange}>
      <PopoverTrigger asChild>
        <Button
          ref={layout.triggerRef}
          variant="ghost"
          size="sm"
          /* `shrink` is load-bearing, not decorative: see Composer.test.tsx. */
          className="nex-composer-model-trigger shrink"
          aria-label={t("conv.modePickerAria")}
          title={selectedModel ?? t("conv.modelTagTitle")}
        >
          <span
            className={
              selectedModel === null
                ? "nex-composer-model-name is-placeholder"
                : "nex-composer-model-name"
            }
          >
            {selectedModel ?? t("conv.modelNone")}
          </span>
          {effectiveEffort && (
            <span className="nex-composer-model-effort-pill" aria-hidden="true">
              {formatEffortLabel(effectiveEffort)}
            </span>
          )}
          <ChevronDownIcon />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        side="top"
        sideOffset={layout.sideOffset}
        collisionPadding={8}
        className="nex-composer-model-menu"
        style={
          layout.maxHeight === undefined
            ? undefined
            : { maxHeight: layout.maxHeight }
        }
        onOpenAutoFocus={(event) => {
          if (!noModels) {
            event.preventDefault();
            inputRef.current?.focus();
          }
        }}
      >
        <PopoverHeader>
          <PopoverTitle>{t("conv.modelSection")}</PopoverTitle>
        </PopoverHeader>

        {!noModels && (
          <div className="nex-composer-model-search">
            <SearchIcon
              className="nex-composer-model-search-icon"
              aria-hidden="true"
            />
            <input
              ref={inputRef}
              type="search"
              value={query}
              placeholder={t("conv.modelSearch")}
              aria-label={t("conv.modelSearch")}
              aria-controls={listId}
              aria-activedescendant={
                filtered.length > 0 ? optionId(active) : undefined
              }
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={onKeyDown}
            />
          </div>
        )}

        {noModels ? (
          <p className="nex-composer-model-empty">{t("conv.modelEmpty")}</p>
        ) : noMatch ? (
          <p className="nex-composer-model-empty">{t("conv.modelNoMatch")}</p>
        ) : (
          <ul
            id={listId}
            role="listbox"
            aria-label={t("conv.modelSection")}
            className="nex-composer-model-list"
          >
            {filtered.map((entry, index) => {
              const previousConnectionId =
                index > 0 ? filtered[index - 1]?.connectionId : null;
              const showGroupHeader =
                groupedMode &&
                entry.connectionId !== null &&
                entry.connectionId !== previousConnectionId;
              const selected =
                entry.model === selectedModel &&
                (!groupedMode ||
                  !selectedConnectionId ||
                  entry.connectionId === selectedConnectionId);
              return (
                <Fragment key={entry.key}>
                  {showGroupHeader && (
                    <li
                      role="presentation"
                      className="nex-composer-model-group-header"
                      aria-hidden="true"
                    >
                      <span>{entry.connectionName ?? entry.connectionId}</span>
                    </li>
                  )}
                  <li
                    id={optionId(index)}
                    role="option"
                    aria-selected={selected}
                    data-active={index === active ? "true" : undefined}
                    data-connection-id={entry.connectionId ?? undefined}
                    className="nex-composer-model-option"
                    onMouseEnter={() => setActive(index)}
                    onClick={() => pick(entry)}
                  >
                    <span
                      className="nex-composer-model-option-name"
                      title={entry.model}
                    >
                      {entry.model}
                    </span>
                    {selected && (
                      <CheckIcon
                        className="nex-composer-model-option-check"
                        aria-hidden="true"
                      />
                    )}
                  </li>
                </Fragment>
              );
            })}
          </ul>
        )}

        {selectedModel !== null && activeCapabilities !== null && (
          <div
            className="nex-composer-model-effort"
            data-effort-supported={
              supportedEfforts.length > 0 ? "true" : "false"
            }
            data-supported-efforts={supportedEfforts.join(",")}
          >
            <div className="nex-composer-model-effort-header">
              <span className="nex-composer-model-effort-title">
                {t("conv.effortLabel")}
              </span>
              <span className="nex-composer-model-provenance">
                {formatProvenance(activeCapabilities.provenance)}
              </span>
            </div>

            {supportedEfforts.length === 0 ? (
              <p
                className="nex-composer-model-effort-unsupported"
                data-testid="effort-unsupported"
              >
                {t("conv.effortUnsupported")}
              </p>
            ) : (
              <>
                {supportedEfforts.length > 1 && currentEffortIndex >= 0 && (
                  <Slider
                    min={0}
                    max={supportedEfforts.length - 1}
                    step={1}
                    value={[currentEffortIndex]}
                    disabled={!onSelectEffort}
                    thumbProps={{ "aria-label": t("conv.effortLabel") }}
                    onValueChange={([nextIndex]) => {
                      const nextLevel =
                        nextIndex !== undefined
                          ? supportedEfforts[nextIndex]
                          : undefined;
                      if (nextLevel && onSelectEffort) {
                        onSelectEffort(nextLevel);
                      }
                    }}
                  />
                )}
                <div
                  className="nex-composer-model-effort-steps"
                  role="group"
                  aria-label={t("conv.effortLabel")}
                >
                  {supportedEfforts.map((level) => {
                    const activeStep = level === effectiveEffort;
                    return (
                      <button
                        key={level}
                        type="button"
                        className={
                          activeStep
                            ? "nex-composer-effort-step is-active"
                            : "nex-composer-effort-step"
                        }
                        aria-pressed={activeStep}
                        disabled={!onSelectEffort}
                        onClick={() => onSelectEffort?.(level)}
                      >
                        {formatEffortLabel(level)}
                      </button>
                    );
                  })}
                </div>
                {activeCapabilities.has_manual_override && (
                  <span className="nex-composer-model-override-note">
                    {t("conv.effortOverrideHint")}
                  </span>
                )}
              </>
            )}
          </div>
        )}

        {modelError && (
          <p className="nex-composer-model-empty" role="alert">
            {modelError || t("conv.modelSwitchFailed")}
          </p>
        )}

        <div className="nex-composer-model-footer">
          <Button variant="ghost" size="xs" onClick={onOpenSettings}>
            {t("conv.modelOpenSettings")}
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  );
}
