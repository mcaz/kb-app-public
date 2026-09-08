import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setupI18n } from "@/i18n";
import {
  api,
  type DistillationAiSettings,
  type DistillationModelCatalog,
  type DistillationQueueView,
  type ImmediateDistillationResult,
} from "@/lib/api";
import { queryKeys } from "@/lib/queries";

import { DistillationSettingsPage } from "./DistillationSettingsPage";

vi.mock("@/lib/api", async () => {
  const { isKbError } = await import("@/lib/api/error");
  return {
    IN_TAURI: false,
    isKbError,
    api: {
      settingsGet: vi.fn(),
      distillationSettingsGet: vi.fn(),
      distillationSettingsSet: vi.fn(),
      distillationProviders: vi.fn(),
      distillationModels: vi.fn(),
      distillationQueueStatus: vi.fn(),
      distillationRequestNow: vi.fn(),
      distillationRetryFailed: vi.fn(),
    },
  };
});

const saved: DistillationAiSettings = {
  enabled: true,
  provider: "codex",
  model: "gpt-5.6-sol",
  reasoning_effort: "ultra",
  timeout_seconds: 300,
  periodic_hours: 168,
};
const catalog: DistillationModelCatalog = {
  provider: "codex",
  unavailable_reason: null,
  models: [
    {
      model: "gpt-5.6-sol",
      display_name: "Sol",
      supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max", "ultra"],
      default_reasoning_effort: "medium",
      is_default: true,
    },
    {
      model: "gpt-5.6-terra",
      display_name: "Terra",
      supported_reasoning_efforts: ["low", "medium", "high", "ultra"],
      default_reasoning_effort: "medium",
      is_default: false,
    },
  ],
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((finish) => {
    resolve = finish;
  });
  return { promise, resolve };
}

let client: QueryClient;
let stored: DistillationAiSettings;
const originalScrollIntoView = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  "scrollIntoView",
);
beforeEach(() => {
  setupI18n("ja");
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe = vi.fn();
      unobserve = vi.fn();
      disconnect = vi.fn();
    },
  );
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
    configurable: true,
    value: vi.fn(),
  });
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  stored = { ...saved };
  vi.mocked(api.settingsGet).mockResolvedValue({
    ai_kb_enabled: true,
    claude_kb_enabled: true,
    gpt_kb_enabled: true,
  });
  vi.mocked(api.distillationSettingsGet).mockImplementation(() => Promise.resolve({ ...stored }));
  vi.mocked(api.distillationSettingsSet).mockImplementation((settings) => {
    stored = { ...settings };
    return Promise.resolve({ ...stored });
  });
  vi.mocked(api.distillationProviders).mockResolvedValue([
    { provider: "codex", installed: true, unavailable_reason: null },
    { provider: "claude_code", installed: true, unavailable_reason: null },
  ]);
  vi.mocked(api.distillationModels).mockResolvedValue(catalog);
  vi.mocked(api.distillationQueueStatus).mockResolvedValue({
    paused: false,
    metrics: { available: true, runs: [] },
    jobs: {
      pending: 1,
      running: 1,
      retry_wait: 0,
      blocked: 0,
      completed: 2,
      oldest_pending_at: null,
    },
    issues: [],
  });
});
afterEach(() => {
  cleanup();
  client.clear();
  if (originalScrollIntoView) {
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", originalScrollIntoView);
  } else {
    Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
  }
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

const show = () =>
  render(
    <QueryClientProvider client={client}>
      <DistillationSettingsPage />
    </QueryClientProvider>,
  );

function expectSavedSelection(model = "Sol", effort = "Ultra") {
  expect(screen.getByLabelText("モデル")).toHaveTextContent(model);
  expect(screen.getByLabelText("推論レベル")).toHaveTextContent(effort);
  expect(screen.queryByRole("button", { name: "やめる" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "未処理を蒸留" })).toBeEnabled();
}

async function choose(label: string, option: string) {
  fireEvent.keyDown(screen.getByLabelText(label), { key: "ArrowDown" });
  fireEvent.click(await screen.findByRole("option", { name: option }));
}

async function showReady() {
  client.setQueryData(queryKeys.distillationModels("codex"), catalog);
  const view = show();
  await waitFor(() => expectSavedSelection());
  return view;
}

// 2026-09-07: 設定を開き直すだけで、モデル空欄・推論レベル既定の未保存変更が現れた。
describe("distillation settings selection stability", () => {
  it("遅れて届くモデル一覧で保存済みモデルと推論レベルを変更しない", async () => {
    const pending = deferred<DistillationModelCatalog>();
    vi.mocked(api.distillationModels).mockReturnValue(pending.promise);
    show();
    expect(await screen.findByDisplayValue("gpt-5.6-sol")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "未処理を蒸留" })).toBeEnabled());
    await act(async () => {
      pending.resolve(catalog);
      await pending.promise;
    });
    await waitFor(() => expectSavedSelection());
    expect(api.distillationSettingsSet).not.toHaveBeenCalled();
    expect(stored).toEqual(saved);
  });

  it("設定を開き直して一覧を取り直しても未保存変更を作らない", async () => {
    const view = await showReady();
    view.unmount();
    client.removeQueries({ queryKey: queryKeys.distillationModels("codex"), exact: true });
    const pending = deferred<DistillationModelCatalog>();
    vi.mocked(api.distillationModels).mockReturnValue(pending.promise);
    show();
    expect(await screen.findByDisplayValue("gpt-5.6-sol")).toBeInTheDocument();
    await act(async () => {
      pending.resolve(catalog);
      await pending.promise;
    });
    await waitFor(() => expectSavedSelection());
    expect(api.distillationSettingsSet).not.toHaveBeenCalled();
    expect(stored).toEqual(saved);
  });

  it("一覧の更新や一時的な候補欠落でも保存済みの値を維持する", async () => {
    await showReady();
    fireEvent.click(screen.getByRole("button", { name: "モデル一覧を更新" }));
    await waitFor(() => expect(api.distillationModels).toHaveBeenCalledOnce());
    await waitFor(() => expectSavedSelection());

    vi.mocked(api.distillationModels).mockResolvedValue({ ...catalog, models: [] });
    fireEvent.click(screen.getByRole("button", { name: "モデル一覧を更新" }));
    expect(await screen.findByDisplayValue("gpt-5.6-sol")).toBeInTheDocument();
    expect(screen.getByLabelText("推論レベル")).toHaveTextContent("Ultra");
    expect(screen.queryByRole("button", { name: "やめる" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "未処理を蒸留" })).toBeEnabled();

    vi.mocked(api.distillationModels).mockResolvedValue(catalog);
    fireEvent.click(screen.getByRole("button", { name: "モデル一覧を更新" }));
    await waitFor(() => expectSavedSelection());
    expect(api.distillationSettingsSet).not.toHaveBeenCalled();
    expect(stored).toEqual(saved);
  });

  it("同じAI・モデル・推論レベルを選び直しても値をリセットしない", async () => {
    await showReady();
    await choose("実行するAI", "Codex");
    await choose("モデル", "Sol");
    await choose("推論レベル", "Ultra");
    expectSavedSelection();
    expect(api.distillationSettingsSet).not.toHaveBeenCalled();
  });

  it("モデル変更は未保存に留め、キャンセルすると元のモデルと推論レベルへ戻る", async () => {
    await showReady();
    await choose("モデル", "Terra");
    expect(screen.getByLabelText("モデル")).toHaveTextContent("Terra");
    expect(screen.getByLabelText("推論レベル")).toHaveTextContent("モデルの既定設定");
    expect(screen.getByRole("button", { name: "保存" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "未処理を蒸留" })).toBeDisabled();
    expect(api.distillationSettingsSet).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "やめる" }));
    await waitFor(() => expectSavedSelection());
    expect(stored).toEqual(saved);
  });

  it("正規のモデル・推論レベル変更を保存し、開き直しても保持する", async () => {
    const view = await showReady();
    await choose("モデル", "Terra");
    await choose("推論レベル", "High");
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expectSavedSelection("Terra", "High"));
    expect(api.distillationSettingsSet).toHaveBeenCalledOnce();
    expect(vi.mocked(api.distillationSettingsSet).mock.calls[0]?.[0]).toEqual({
      ...saved,
      model: "gpt-5.6-terra",
      reasoning_effort: "high",
    });
    view.unmount();
    show();
    await waitFor(() => expectSavedSelection("Terra", "High"));
    expect(api.distillationSettingsSet).toHaveBeenCalledOnce();
  });

  it("カスタム識別子を編集して保存できる", async () => {
    await showReady();
    await choose("モデル", "その他のモデルを指定");
    fireEvent.change(screen.getByLabelText("モデルの識別子"), {
      target: { value: "test-custom-model" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "やめる" })).not.toBeInTheDocument(),
    );
    expect(screen.getByLabelText("モデルの識別子")).toHaveValue("test-custom-model");
    expect(screen.getByRole("button", { name: "未処理を蒸留" })).toBeEnabled();
    expect(api.distillationSettingsSet).toHaveBeenCalledOnce();
    expect(vi.mocked(api.distillationSettingsSet).mock.calls[0]?.[0]).toEqual({
      ...saved,
      model: "test-custom-model",
      reasoning_effort: null,
    });
  });

  it("計測情報の更新で保存済みのモデル・推論レベルを変更しない", async () => {
    await showReady();
    act(() => {
      client.setQueryData<DistillationQueueView>(queryKeys.distillationQueue, (queue) =>
        queue
          ? {
              ...queue,
              metrics: {
                available: true,
                runs: [
                  {
                    run_id: "test-poll-update",
                    provider: "codex",
                    model: "test-historical-model",
                    reasoning_effort: "high",
                    attempt: 1,
                    generation: 1,
                    batch_size: 6,
                    completed_notes: 6,
                    input_bytes: 48_000,
                    started_at_ms: 1_700_000_000_000,
                    finished_at_ms: 1_700_000_012_000,
                    elapsed_ms: 12_000,
                    elapsed_is_estimate: false,
                    outcome: "no_change",
                    failure: null,
                    stages: [],
                  },
                ],
              },
            }
          : queue,
      );
    });
    expect(await screen.findByText("変更なしで完了")).toBeInTheDocument();
    expectSavedSelection();
    expect(api.distillationSettingsSet).not.toHaveBeenCalled();
    expect(stored).toEqual(saved);
  });
});

describe("distillation execution controls", () => {
  it.each([
    ["未処理を蒸留", "unreviewed"],
    ["全体を見直す", "all"],
  ] as const)(
    "%sは実行中のノートがあっても受付でき、要求中の重複送信を防ぐ",
    async (label, scope) => {
      const pending = deferred<ImmediateDistillationResult>();
      vi.mocked(api.distillationRequestNow).mockReturnValue(pending.promise);
      await showReady();

      fireEvent.click(screen.getByRole("button", { name: label }));
      await waitFor(() => expect(api.distillationRequestNow).toHaveBeenCalledOnce());
      expect(vi.mocked(api.distillationRequestNow).mock.calls[0]?.[0]).toBe(scope);
      expect(screen.getByRole("button", { name: "受け付けています…" })).toBeDisabled();
      expect(
        screen.getByRole("button", { name: scope === "all" ? "未処理を蒸留" : "全体を見直す" }),
      ).toBeDisabled();
      fireEvent.click(screen.getByRole("button", { name: "受け付けています…" }));
      expect(api.distillationRequestNow).toHaveBeenCalledOnce();

      await act(async () => {
        pending.resolve({
          registered: 0,
          requeued: 0,
          expedited: 1,
          jobs: {
            pending: 1,
            running: 1,
            retry_wait: 0,
            blocked: 0,
            completed: 2,
            oldest_pending_at: null,
          },
        });
        await pending.promise;
      });
      await waitFor(() => expectSavedSelection());
      expect(api.distillationSettingsSet).not.toHaveBeenCalled();
    },
  );

  it("選択したAIのKB利用がOFFなら進捗を伏せ、即時実行と再試行を停止する", async () => {
    vi.mocked(api.settingsGet).mockResolvedValue({
      ai_kb_enabled: true,
      claude_kb_enabled: true,
      gpt_kb_enabled: false,
    });
    show();
    const progress = await screen.findByRole("region", { name: "処理の状況" });
    await waitFor(() => expect(progress).toHaveTextContent("選んだAIのKB利用がOFFのため停止中"));
    expect(progress.querySelector("dl")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "未処理を蒸留" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "全体を見直す" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "失敗した処理を再試行" })).toBeDisabled();
    expect(api.distillationRequestNow).not.toHaveBeenCalled();
    expect(api.distillationRetryFailed).not.toHaveBeenCalled();
  });
});
