import { useEffect, useState } from "react";
import type { GatewayApi } from "./gateway";
import type { GatewayModel } from "./types";

const EMPTY_MODELS: GatewayModel[] = [];

// Keep a previous project's results invisible, including before the effect runs.
export function useProjectModels(api: GatewayApi | undefined, cwd: string | undefined, revision: number) {
  const [state, setState] = useState<{
    api: GatewayApi; cwd: string; revision: number;
    models: GatewayModel[]; loading: boolean; error: string | null;
  } | null>(null);
  useEffect(() => {
    if (!api || !cwd) return;
    let cancelled = false;
    setState({ api, cwd, revision, models: [], loading: true, error: null });
    void api.models(undefined, cwd).then(
      models => { if (!cancelled) setState({ api, cwd, revision, models, loading: false, error: null }); },
      error => { if (!cancelled) setState({ api, cwd, revision, models: [], loading: false, error: String(error) }); },
    );
    return () => { cancelled = true; };
  }, [api, cwd, revision]);
  return state?.api === api && state?.cwd === cwd && state?.revision === revision
    ? state
    : { models: EMPTY_MODELS, loading: Boolean(api && cwd), error: null };
}
