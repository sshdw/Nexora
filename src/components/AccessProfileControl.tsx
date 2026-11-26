import { useId, useRef, useState } from "react";
import type { AccessProfile } from "../lib/tauri";
import type { AccessProfileStore } from "../lib/useAccessProfile";
import { useStrings } from "../lib/useLocale";
import SegmentedGroup from "./SegmentedGroup";
import ComposerAutonomyHint from "./ComposerAutonomyHint";
import ConfirmDialog from "./ConfirmDialog";
import {
  CheckIcon,
  HandIcon,
  ShieldAlertIcon,
  ShieldCheckIcon,
} from "./icons";

const ACCESS_PROFILES: readonly AccessProfile[] = [
  "supervised",
  "standard",
  "full_access",
];

function AccessProfileIcon({ profile }: { profile: AccessProfile }) {
  if (profile === "supervised") return <HandIcon size={16} />;
  if (profile === "standard") return <ShieldCheckIcon size={16} />;
  return <ShieldAlertIcon size={16} />;
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
  const [confirmFullAccessOpen, setConfirmFullAccessOpen] = useState(false);
  const disabled = store.loading || store.saving;

  const requestProfile = (target: AccessProfile) => {
    if (
      target === "full_access" &&
      !store.fullAccessAcknowledged &&
      store.profile !== "full_access"
    ) {
      setConfirmFullAccessOpen(true);
      return;
    }
    void store.setProfile(target);
  };

  const handleConfirmFullAccess = async () => {
    const err = await store.setProfile("full_access", {
      acknowledgeFullAccess: true,
    });
    if (!err) {
      setConfirmFullAccessOpen(false);
    }
  };

  const confirmModal = confirmFullAccessOpen ? (
    <ConfirmDialog
      title={t("access.fullAccessConfirmTitle")}
      body={t("access.fullAccessConfirmBody")}
      confirmLabel={t("access.fullAccessConfirmAction")}
      danger
      busy={store.saving}
      onConfirm={() => void handleConfirmFullAccess()}
      onCancel={() => setConfirmFullAccessOpen(false)}
    />
  ) : null;

  if (variant === "list") {
    const moveFocus = (nextIdx: number) => {
      const target = ACCESS_PROFILES[nextIdx];
      if (!target) return;
      optionRefs.current[nextIdx]?.focus();
      requestProfile(target);
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
                className={
                  profile === "full_access"
                    ? "nex-access-option nex-access-option-warn"
                    : "nex-access-option"
                }
                onClick={() => requestProfile(profile)}
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
        {confirmModal}
      </div>
    );
  }

  return (
    <div className="nex-settings-field" aria-busy={disabled}>
      {showLabel && (
        <span className="nex-settings-label">{t("access.label")}</span>
      )}
      <SegmentedGroup
        className={segmentedClassName}
        label={t("access.label")}
        semantics="radio"
        value={store.profile}
        onChange={requestProfile}
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
      {confirmModal}
    </div>
  );
}
