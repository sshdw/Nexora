//! One lifted persisted profile source. Failed reads fail closed; failed
//! writes roll back both Composer and Settings together.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getAccessProfile,
  setAccessProfile,
  type AccessProfile,
} from "./tauri";
import { getLocale, tr } from "./strings";
export interface AccessProfileStore {
  profile: AccessProfile;
  loading: boolean;
  saving: boolean;
  error: string | null;
  reload: () => Promise<void>;
  setProfile: (profile: AccessProfile) => Promise<string | null>;
}
export function useAccessProfile(): AccessProfileStore {
  const [profile, sync] = useState<AccessProfile>("standard");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = useRef<AccessProfile>("standard");
  const writing = useRef(false);
  const revision = useRef(0);
  const update = useCallback((next: AccessProfile) => {
    current.current = next;
    sync(next);
  }, []);
  const reload = useCallback(async () => {
    if (writing.current) return;
    const generation = ++revision.current;
    setLoading(true);
    try {
      const next = await getAccessProfile();
      if (generation === revision.current) update(next);
    } catch {
      if (generation === revision.current) update("read_only");
    } finally {
      if (generation === revision.current) setLoading(false);
    }
  }, [update]);
  const setProfile = useCallback(
    async (next: AccessProfile) => {
      if (writing.current) return tr(getLocale(), "access.saving");
      writing.current = true;
      ++revision.current;
      const previous = current.current;
      update(next);
      setSaving(true);
      setError(null);
      setLoading(false);
      try {
        await setAccessProfile(next);
        return null;
      } catch {
        update(previous);
        const message = tr(getLocale(), "access.saveFailed");
        setError(message);
        return message;
      } finally {
        writing.current = false;
        setSaving(false);
      }
    },
    [update]
  );
  useEffect(() => {
    void reload();
  }, [reload]);
  return useMemo(
    () => ({ profile, loading, saving, error, reload, setProfile }),
    [profile, loading, saving, error, reload, setProfile]
  );
}
