import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

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
import { FilesPage } from "@/pages/FilesPage";
import { HomePage } from "@/pages/HomePage";
import { NotesPage } from "@/pages/NotesPage";
import { OnboardingPage } from "@/pages/OnboardingPage";
import { ProposalsPage } from "@/pages/ProposalsPage";
import { SettingsDialog } from "@/pages/SettingsDialog";
import {
  queryKeys,
  useHomeState,
  useMaintenanceRefresh,
  useNoteCategories,
  useSetupState,
} from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

export function App() {
  const { t } = useTranslation("common");
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
  const view = useSession((s) => s.view);
  const selectedId = useSession((s) => s.selectedId);
  const [searchOpen, setSearchOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
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
  if (setup?.needs_onboarding) return <OnboardingPage />;

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
        {view === "graph" && <GraphPage />}
        {view === "proposals" && <ProposalsPage />}
      </AppShell>

      <GlobalSearchDialog
        key={searchOpening}
        mode={searchMode}
        open={searchOpen}
        onOpenChange={setSearchOpen}
      />
      <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} />
      <Toaster theme={theme} />
    </TooltipProvider>
  );
}
