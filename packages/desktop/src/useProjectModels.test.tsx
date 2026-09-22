/* @vitest-environment jsdom */
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import type { GatewayApi } from "./gateway";
import type { GatewayModel } from "./types";
import { useProjectModels } from "./useProjectModels";

it("isolates projects, rejects late responses, refreshes and recovers after failure", async () => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const pending: Array<{ resolve: (models: GatewayModel[]) => void; reject: (error: Error) => void }> = [];
  const api = { models: vi.fn(() => new Promise<GatewayModel[]>((resolve, reject) => pending.push({ resolve, reject }))) } as unknown as GatewayApi;
  const container = document.createElement("div");
  const root = createRoot(container);
  function View({ cwd, revision = 0, client = api }: { cwd: string; revision?: number; client?: GatewayApi }) {
    const state = useProjectModels(client, cwd, revision);
    return <div>{state.loading ? "loading" : state.error ?? state.models.map(m => m.name).join(",")}</div>;
  }
  const model = (name: string) => ({ name, description: name, group: "default" });
  try {
    await act(async () => root.render(<View cwd="D:/A" />));
    expect(api.models).toHaveBeenLastCalledWith(undefined, "D:/A");
    await act(async () => root.render(<View cwd="D:/B" />));
    await act(async () => pending[1].resolve([model("B")]));
    await act(async () => pending[0].resolve([model("A")]));
    expect(container.textContent).toBe("B");
    await act(async () => root.render(<View cwd="D:/B" revision={1} />));
    expect(container.textContent).toBe("loading");
    await act(async () => pending[2].reject(new Error("offline")));
    expect(container.textContent).toContain("offline");
    expect(container.textContent).not.toContain("B");
    await act(async () => root.render(<View cwd="D:/B" revision={2} />));
    await act(async () => pending[3].resolve([model("B"), model("Mimo")]));
    expect(container.textContent).toBe("B,Mimo");
    const reconnected = { models: vi.fn().mockResolvedValue([model("reconnected")]) } as unknown as GatewayApi;
    await act(async () => root.render(<View cwd="D:/B" client={reconnected} />));
    expect(container.textContent).toBe("reconnected");
  } finally {
    await act(async () => root.unmount());
  }
});
