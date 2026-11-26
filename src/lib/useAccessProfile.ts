//! One lifted persisted profile source. Failed reads fail closed; failed
//! writes roll back both Composer and Settings together.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getAccessProfile,
  getSetting,
  setAccessProfile,
  setSetting,
  type AccessProfile,
} from "./tauri";
import { getLocale, tr } from "./strings";

export const FULL_ACCESS_ACKNOWLEDGED_KEY = "agent.full_access_acknowledged";

export interface SetAccessProfileOptions {
  acknowledgeFullAccess?: boolean;
}

export interface AccessProfileStore {
  profile: AccessProfile;
  fullAccessAcknowledged?: boolean;
  loading: boolean;
  saving: boolean;
  error: string | null;
  reload: () => Promise<void>;
  setProfile: (
    profile: AccessProfile,
    options?: SetAccessProfileOptions
  ) => Promise<string | null>;
}

export function useAccessProfile(): AccessProfileStore {
  const [profile, sync] = useState<AccessProfile>("standard");
  const [fullAccessAcknowledged, syncAck] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = useRef<AccessProfile>("standard");
  const ackRef = useRef(false);
  const writing = useRef(false);
  const revision = useRef(0);
  const update = useCallback((next: AccessProfile) => {
    current.current = next;
    sync(next);
  }, []);
  const updateAck = useCallback((next: boolean) => {
    ackRef.current = next;
    syncAck(next);
  }, []);
  const reload = useCallback(async () => {
    if (writing.current) return;
    const generation = ++revision.current;
    setLoading(true);
    try {
      const [next, rawAck] = await Promise.all([
        getAccessProfile(),
        getSetting(FULL_ACCESS_ACKNOWLEDGED_KEY),
      ]);
      const acknowledged = rawAck?.trim().toLowerCase() === "true";
      if (generation === revision.current) {
        updateAck(acknowledged);
        update(next === "full_access" && !acknowledged ? "standard" : next);
      }
    } catch {
      if (generation === revision.current) {
        updateAck(false);
        update("supervised");
      }
    } finally {
      if (generation === revision.current) setLoading(false);
    }
  }, [update, updateAck]);
  const setProfile = useCallback(
    async (next: AccessProfile, options?: SetAccessProfileOptions) => {
      if (writing.current) return tr(getLocale(), "access.saving");
      writing.current = true;
      ++revision.current;
      const previous = current.current;
      const previousAck = ackRef.current;
      update(next);
      setSaving(true);
      setError(null);
      setLoading(false);
      try {
        if (next === "full_access" && options?.acknowledgeFullAccess) {
          await setSetting(FULL_ACCESS_ACKNOWLEDGED_KEY, "true");
          updateAck(true);
        }
        await setAccessProfile(next);
        return null;
      } catch {
        update(previous);
        updateAck(previousAck);
        const message = tr(getLocale(), "access.saveFailed");
        setError(message);
        return message;
      } finally {
        writing.current = false;
        setSaving(false);
      }
    },
    [update, updateAck]
  );
  useEffect(() => {
    void reload();
  }, [reload]);
  return useMemo(
    () => ({
      profile,
      fullAccessAcknowledged,
      loading,
      saving,
      error,
      reload,
      setProfile,
    }),
    [profile, fullAccessAcknowledged, loading, saving, error, reload, setProfile]
  );
}
