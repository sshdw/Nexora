//! Model picker (R6-02).
//!
//! The trigger names the store's selected model; the popover is a REAL
//! searchable list. Round 5 reused the segmented primitive here, and that
//! primitive's root is an `inline-grid` child of `.nex-seg`: its implicit
//! column resolved to max-content, so the list grew wider than the 260 px
//! popover and the UA painted a horizontal scrollbar. The list is now a
//! plain `<ul role="listbox">` with `overflow-x: hidden` and wrapping ids,
//! so no window width can produce a horizontal bar.
//!
//! Behaviour is one owner:
//!   - the search field filters model ids case-insensitively, trimmed;
//!   - ArrowUp/Down/Home/End move the active option (the input keeps
//!     focus and `aria-activedescendant` names the row);
//!   - Enter picks the active row, Escape closes;
//!   - clicking a row picks it and closes.
//!
//! The component DISPATCHES `onSelectModel` and keeps no copy of the
//! selection: the trigger re-renders from the prop when the provider store
//! answers. Two empty states are distinct on purpose — "no models
//! configured" is a setup problem, "no search match" is a query problem.

import { useEffect, useId, useMemo, useRef, useState } from "react";

import { useStrings } from "../lib/useLocale";
import { Button } from "./ui/button";
import {
  Popover,
  PopoverContent,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
} from "./ui/popover";
import { CheckIcon, ChevronDownIcon, SearchIcon } from "./icons";

export interface ModelPickerProps {
  /** Selected model, or null when none is configured. */
  selectedModel: string | null;
  /** Models offered by the selected provider. */
  models: readonly string[];
  /** Persist a model choice. The provider store owns the result. */
  onSelectModel: (model: string) => void;
  /** Last model-selection failure, surfaced inside the popover. */
  modelError?: string | null;
  /** Deep link to the provider section of Settings. */
  onOpenSettings: () => void;
}

export default function ModelPicker({
  selectedModel,
  models,
  onSelectModel,
  modelError = null,
  onOpenSettings,
}: ModelPickerProps) {
  const { t } = useStrings();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listId = useId();
  const optionId = (index: number): string => `${listId}-option-${index}`;

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (needle === "") return models;
    return models.filter((model) => model.toLowerCase().includes(needle));
  }, [models, query]);

  // Opening (or changing the query) puts the active row on the current
  // selection when it survived the filter, otherwise on the first row.
  useEffect(() => {
    if (!open) return;
    const selected = filtered.findIndex((model) => model === selectedModel);
    setActive(selected >= 0 ? selected : 0);
  }, [open, filtered, selectedModel]);

  const handleOpenChange = (next: boolean) => {
    setOpen(next);
    if (!next) setQuery("");
  };

  const pick = (model: string) => {
    onSelectModel(model);
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
        const model = filtered[active];
        if (model !== undefined) pick(model);
        break;
      }
      case "Escape":
        event.preventDefault();
        setOpen(false);
        setQuery("");
        break;
    }
  };

  const noModels = models.length === 0;
  const noMatch = !noModels && filtered.length === 0;

  return (
    <Popover open={open} onOpenChange={handleOpenChange}>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="nex-composer-model-trigger"
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
          <ChevronDownIcon />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        side="top"
        sideOffset={8}
        collisionPadding={8}
        className="nex-composer-model-menu"
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
            <SearchIcon className="nex-composer-model-search-icon" aria-hidden="true" />
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
            {filtered.map((model, index) => {
              const selected = model === selectedModel;
              return (
                <li
                  key={model}
                  id={optionId(index)}
                  role="option"
                  aria-selected={selected}
                  data-active={index === active ? "true" : undefined}
                  className="nex-composer-model-option"
                  onMouseEnter={() => setActive(index)}
                  onClick={() => pick(model)}
                >
                  <span className="nex-composer-model-option-name" title={model}>
                    {model}
                  </span>
                  {selected && (
                    <CheckIcon className="nex-composer-model-option-check" aria-hidden="true" />
                  )}
                </li>
              );
            })}
          </ul>
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
