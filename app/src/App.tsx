import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Toaster } from "@/components/atoms/ui/sonner";
import { TooltipProvider } from "@/components/atoms/ui/tooltip";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { GlobalSearchDialog } from "@/components/organisms/GlobalSearchDialog";
import { Sidebar } from "@/components/organisms/Sidebar";
import { WorkspaceTabs } from "@/components/organisms/WorkspaceTabs";
import { AppShell } from "@/components/templates/AppShell";
import { useAutomaticRefresh } from "@/hooks/useAutomaticRefresh";
import { useErrorText } from "@/hooks/useErrorText";
import { useNoteFileIntake } from "@/hooks/useNoteFileIntake";
import { useGlobalSearchShortcut } from "@/hooks/useGlobalSearchShortcut";
import { useNavigationHistoryShortcut } from "@/hooks/useNavigationHistoryShortcut";
import { useSettingsShortcut } from "@/hooks/useSettingsShortcut";
import { useTheme } from "@/hooks/useTheme";
import { useTrayLabels } from "@/hooks/useTrayLabels";
import { useWorkspaceTabShortcuts } from "@/hooks/useWorkspaceTabShortcuts";
import { GraphPage } from "@/pages/GraphPage";
import { TagsPage } from "@/pages/TagsPage";
import { FilesPage } from "@/pages/FilesPage";
import { HomePage } from "@/pages/HomePage";
import { NotesPage } from "@/pages/NotesPage";
import { OnboardingPage } from "@/pages/OnboardingPage";
import { ProposalsPage } from "@/pages/ProposalsPage";
import { SettingsDialog, type SettingsSection } from "@/pages/SettingsDialog";
import {
  queryKeys,
  useHomeState,
  useMaintenanceRefresh,
  useNoteCategories,
  useSetupState,
  useAppUpdateStatus,
} from "@/lib/queries";
import { useSession } from "@/lib/stores/session";
import { api } from "@/lib/api";
import { appUpdateBootReadyAllowed } from "@/lib/queries/appUpdatePolicy";

export function App() {
  const { t } = useTranslation(["common", "appUpdate"]);
  const errorText = useErrorText();
  const {
    data: setup,
    error: setupError,
    isError: setupFailed,
    isPending,
    refetch: retrySetup,
  } = useSetupState();
  const ready = setup !== undefined && !setup.needs_onboarding;
  const automaticRefresh = useAutomaticRefresh(ready);
  const homeQuery = useHomeState(ready);
  const home = homeQuery.data;
  const {
    data: categoryData,
    error: categoryError,
    isFetching: categoriesFetching,
    refetch: retryCategories,
  } = useNoteCategories(ready);
  // 初回のDB表示を先に完了させてから保守を開始する(stale-while-revalidate)。
  const maintenance = useMaintenanceRefresh(
    ready && home !== undefined && categoryData !== undefined,
  );
  const queryClient = useQueryClient();
  const handledMaintenanceAt = useRef(0);
  // 起動時の復元結果だけを読み、進捗pollは設定画面の表示中に限る。
  const updateQuery = useAppUpdateStatus(false);
  const restoredUpdateShown = useRef(false);
  const bootReadySent = useRef(false);
  const initialScreenReady = appUpdateBootReadyAllowed({
    setup,
    setupFailed,
    homeLoaded: home !== undefined,
    homeFailed: homeQuery.isError,
    categoriesLoaded: categoryData !== undefined,
    categoriesFailed: categoryError !== null,
  });
  const view = useSession((s) => s.view);
  const selectedId = useSession((s) => s.selectedId);
  const [searchOpen, setSearchOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<SettingsSection>("kb");
  const [searchMode, setSearchMode] = useState<"recent" | "all">("recent");
  const [searchOpening, setSearchOpening] = useState(0);
  const openSearch = useCallback(() => {
    setSearchMode("recent");
    setSettingsOpen(false);
    setSearchOpen(true);
  }, []);
  const openAllNotes = useCallback(() => {
    useSession.getState().resetSearch();
    setSettingsOpen(false);
    setSearchMode("all");
    // 前回の選択・小画面プレビュー・debounceを一緒に捨て、毎回全件から開く。
    setSearchOpening((opening) => opening + 1);
    setSearchOpen(true);
  }, []);
  const openSettings = useCallback(() => {
    setSearchOpen(false);
    setSettingsOpen(true);
  }, []);
  const openGettingStarted = useCallback(() => {
    setSettingsSection("getting_started");
    setSearchOpen(false);
    setSettingsOpen(true);
  }, []);
  const finishOnboarding = useCallback(() => {
    useSession.getState().go("home");
    setSettingsSection("connect");
    setSearchOpen(false);
    setSettingsOpen(true);
  }, []);

  // 選んだテーマ(システム/ライト/ダーク)を <html data-theme> へ反映する
  const theme = useTheme();
  // 常駐中のtrayメニューを、画面と同じ言語に保つ
  useTrayLabels();

  // ノートを開いている間だけ、ペースト・ドロップを添付として受ける
  useNoteFileIntake(selectedId, view === "notes");
  useGlobalSearchShortcut(openSearch, !isPending && !setup?.needs_onboarding);
  useSettingsShortcut(openSettings, !isPending && !setup?.needs_onboarding);
  useNavigationHistoryShortcut(!isPending && !setup?.needs_onboarding);
  useWorkspaceTabShortcuts(ready && !searchOpen && !settingsOpen);

  useEffect(() => {
    if (
      updateQuery.isError ||
      updateQuery.data?.failure !== "restart_failed" ||
      restoredUpdateShown.current
    )
      return;
    restoredUpdateShown.current = true;
    toast(t("appUpdate:failures.restart_failed"), {
      action: {
        label: t("appUpdate:openSettings"),
        onClick: () => {
          setSettingsSection("general");
          setSearchOpen(false);
          setSettingsOpen(true);
        },
      },
    });
  }, [updateQuery.data?.failure, updateQuery.isError, t]);

  // 初期画面の取得完了だけをnative監視へ通知する。KB保存・検索の受入証拠ではない。
  useEffect(() => {
    if (!initialScreenReady || bootReadySent.current) return;
    bootReadySent.current = true;
    void api.appUpdateBootReady().catch(() => {
      // 未受領時のrollbackはnative監視に任せ、画面側で成功に補正しない。
      console.warn(t("appUpdate:bootReadyFailed"));
    });
  }, [initialScreenReady, t]);

  // 外部更新・派生情報の保守が終わった後だけ、影響するDB queryを再取得する。
  useEffect(() => {
    if (
      maintenance.dataUpdatedAt === 0 ||
      maintenance.dataUpdatedAt === handledMaintenanceAt.current
    ) {
      return;
    }
    handledMaintenanceAt.current = maintenance.dataUpdatedAt;
    void Promise.all([
      queryClient.invalidateQueries({ queryKey: queryKeys.home }),
      queryClient.invalidateQueries({ queryKey: queryKeys.noteCountHistory }),
      queryClient.invalidateQueries({ queryKey: queryKeys.tagOverview }),
      queryClient.invalidateQueries({ queryKey: queryKeys.noteCategories }),
      queryClient.invalidateQueries({ queryKey: queryKeys.noteLists }),
      queryClient.invalidateQueries({ queryKey: queryKeys.notes }),
      queryClient.invalidateQueries({ queryKey: queryKeys.searches }),
      queryClient.invalidateQueries({ queryKey: queryKeys.graph }),
      queryClient.invalidateQueries({ queryKey: queryKeys.connect }),
      queryClient.invalidateQueries({ queryKey: queryKeys.proposals }),
    ]);
  }, [maintenance.dataUpdatedAt, queryClient]);

  if (isPending) {
    return (
      <main
        aria-busy="true"
        aria-live="polite"
        style={{
          alignItems: "center",
          background: "var(--color-ground)",
          color: "var(--color-ink)",
          display: "flex",
          fontFamily: "system-ui, sans-serif",
          height: "100vh",
          justifyContent: "center",
        }}
      >
        <div style={{ textAlign: "center" }}>
          <strong style={{ display: "block", fontSize: 20 }}>kb-app</strong>
          <span style={{ color: "var(--color-muted)", display: "block", marginTop: 8 }}>
            {t("state.starting")}
          </span>
        </div>
      </main>
    );
  }
  if (setupFailed) {
    return (
      <main
        role="alert"
        style={{
          alignItems: "center",
          background: "var(--color-ground)",
          color: "var(--color-ink)",
          display: "flex",
          fontFamily: "system-ui, sans-serif",
          height: "100vh",
          justifyContent: "center",
          padding: 32,
        }}
      >
        <div style={{ maxWidth: 520, textAlign: "center" }}>
          <strong style={{ display: "block", fontSize: 20 }}>{t("state.startupFailed")}</strong>
          <p style={{ color: "var(--color-muted)", margin: "12px 0 20px" }}>
            {errorText(setupError)}
          </p>
          <button
            type="button"
            onClick={() => void retrySetup()}
            style={{
              background: "var(--color-ink)",
              border: 0,
              borderRadius: 8,
              color: "var(--color-ground)",
              cursor: "pointer",
              font: "inherit",
              padding: "10px 16px",
            }}
          >
            {t("action.retry")}
          </button>
        </div>
      </main>
    );
  }
  if (setup?.needs_onboarding) return <OnboardingPage onReady={finishOnboarding} />;

  return (
    <TooltipProvider delayDuration={200}>
      <AppShell
        banner={
          <DegradedBanner
            items={[
              ...(maintenance.data?.degraded ?? []),
              ...(home?.degraded ?? []),
              ...(categoryData?.degraded ?? []),
            ]}
          />
        }
        sidebar={
          <Sidebar
            categories={categoryData?.categories}
            categoriesError={categoryError ? errorText(categoryError) : null}
            categoriesFetching={categoriesFetching}
            onRetryCategories={() => void retryCategories()}
            onOpenSearch={openSearch}
            settingsOpen={settingsOpen}
            onOpenSettings={openSettings}
          />
        }
        tabs={<WorkspaceTabs />}
      >
        {view === "home" && (
          <HomePage
            home={home}
            onOpenAllNotes={openAllNotes}
            onOpenGettingStarted={openGettingStarted}
            refresh={{
              updatedAt: homeQuery.dataUpdatedAt,
              isFetching: homeQuery.isFetching,
              isError: homeQuery.isError,
              detectionFailed: Boolean(automaticRefresh.error),
              retry: automaticRefresh.retry,
            }}
          />
        )}
        {view === "notes" && <NotesPage onOpenSearch={openSearch} />}
        {view === "files" && <FilesPage />}
        {view === "tags" && <TagsPage />}
        {view === "graph" && <GraphPage />}
        {view === "proposals" && <ProposalsPage />}
      </AppShell>

      <GlobalSearchDialog
        key={searchOpening}
        mode={searchMode}
        open={searchOpen}
        onOpenChange={setSearchOpen}
      />
      <SettingsDialog
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        section={settingsSection}
        onSectionChange={setSettingsSection}
        onOpenSearch={openSearch}
      />
      <Toaster theme={theme} />
    </TooltipProvider>
  );
}
