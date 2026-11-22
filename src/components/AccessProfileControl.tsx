import { useId, useRef } from "react";
import type { AccessProfile } from "../lib/tauri";
import type { AccessProfileStore } from "../lib/useAccessProfile";
import { useStrings } from "../lib/useLocale";
import SegmentedGroup from "./SegmentedGroup";
import ComposerAutonomyHint from "./ComposerAutonomyHint";
import {
  CheckIcon,
  EyeIcon,
  ShieldCheckIcon,
  SlidersIcon,
} from "./icons";

const ACCESS_PROFILES: readonly AccessProfile[] = [
  "read_only",
  "standard",
  "supervised",
];

function AccessProfileIcon({ profile }: { profile: AccessProfile }) {
  if (profile === "read_only") return <EyeIcon size={16} />;
  if (profile === "standard") return <ShieldCheckIcon size={16} />;
  return <SlidersIcon size={16} />;
}

export default function AccessProfileControl({
  store,
  showLabel = true,
  segmentedClassName,
  variant = "segmented",
}: {
  store: AccessProfileStore;
  showLabel?: boolean;
  segmentedClassName?: string;
  variant?: "segmented" | "list";
}) {
  const { t } = useStrings();
  const baseId = useId();
  const optionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const disabled = store.loading || store.saving;

  if (variant === "list") {
    const moveFocus = (nextIdx: number) => {
      const target = ACCESS_PROFILES[nextIdx];
      if (!target) return;
      optionRefs.current[nextIdx]?.focus();
      void store.setProfile(target);
    };

    return (
      <div className="nex-access-list-wrap" aria-busy={disabled}>
        <div
          className="nex-access-list"
          role="radiogroup"
          aria-label={t("access.label")}
        >
          {ACCESS_PROFILES.map((profile, idx) => {
            const selected = store.profile === profile;
            const title = t(`access.${profile}`);
            const desc = t(`access.hint.${profile}`);
            const descId = `${baseId}-${profile}-desc`;
            return (
              <button
                key={profile}
                ref={(el) => {
                  optionRefs.current[idx] = el;
                }}
                type="button"
                role="radio"
                aria-checked={selected}
                aria-label={title}
                aria-describedby={descId}
                tabIndex={selected ? 0 : -1}
                disabled={disabled}
                className="nex-access-option"
                onClick={() => void store.setProfile(profile)}
                onKeyDown={(event) => {
                  if (disabled) return;
                  if (event.key === "ArrowDown" || event.key === "ArrowRight") {
                    event.preventDefault();
                    moveFocus((idx + 1) % ACCESS_PROFILES.length);
                  } else if (
                    event.key === "ArrowUp" ||
                    event.key === "ArrowLeft"
                  ) {
                    event.preventDefault();
                    moveFocus(
                      (idx - 1 + ACCESS_PROFILES.length) %
                        ACCESS_PROFILES.length
                    );
                  } else if (event.key === "Home") {
                    event.preventDefault();
                    moveFocus(0);
                  } else if (event.key === "End") {
                    event.preventDefault();
                    moveFocus(ACCESS_PROFILES.length - 1);
                  }
                }}
              >
                <span className="nex-access-option-icon" aria-hidden="true">
                  <AccessProfileIcon profile={profile} />
                </span>
                <span className="nex-access-option-body">
                  <span className="nex-access-option-title">{title}</span>
                  <span
                    id={descId}
                    className="nex-access-option-desc"
                    title={desc}
                  >
                    {desc}
                  </span>
                </span>
                {selected && (
                  <span className="nex-access-option-check" aria-hidden="true">
                    <CheckIcon size={16} />
                  </span>
                )}
              </button>
            );
          })}
        </div>
        {store.saving && (
          <p className="nex-settings-hint" role="status">
            {t("access.saving")}
          </p>
        )}
        {store.error && (
          <p className="nex-settings-error" role="alert">
            {store.error}
          </p>
        )}
      </div>
    );
  }

  return (
    <div
      className="nex-settings-field"
      aria-busy={disabled}
    >
      {showLabel && (
        <span className="nex-settings-label">{t("access.label")}</span>
      )}
      <SegmentedGroup
        className={segmentedClassName}
        label={t("access.label")}
        semantics="radio"
        value={store.profile}
        onChange={(profile) => void store.setProfile(profile)}
        options={ACCESS_PROFILES.map((profile) => ({
          value: profile,
          label: t(`access.${profile}`),
          disabled,
        }))}
      />
      <ComposerAutonomyHint
        mode={store.profile}
        text={t(`access.hint.${store.profile}`)}
      />
      {store.saving && (
        <p className="nex-settings-hint" role="status">
          {t("access.saving")}
        </p>
      )}
      {store.error && (
        <p className="nex-settings-error" role="alert">
          {store.error}
        </p>
      )}
    </div>
  );
}
