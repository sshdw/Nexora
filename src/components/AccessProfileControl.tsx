import type { AccessProfileStore } from "../lib/useAccessProfile";
import { useStrings } from "../lib/useLocale";
import M3SegmentedGroup from "./M3SegmentedGroup";
import ComposerAutonomyHint from "./ComposerAutonomyHint";
export default function AccessProfileControl({
  store,
  showLabel = true,
}: {
  store: AccessProfileStore;
  showLabel?: boolean;
}) {
  const { t } = useStrings();
  return (
    <div
      className="nex-settings-field"
      aria-busy={store.loading || store.saving}
    >
      {showLabel && (
        <span className="nex-settings-label">{t("access.label")}</span>
      )}
      <M3SegmentedGroup
        label={t("access.label")}
        semantics="radio"
        indicator={false}
        value={store.profile}
        onChange={(profile) => void store.setProfile(profile)}
        options={(["read_only", "standard", "supervised"] as const).map(
          (profile) => ({
            value: profile,
            label: t(`access.${profile}`),
            disabled: store.loading || store.saving,
          })
        )}
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
