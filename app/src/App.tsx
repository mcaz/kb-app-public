import { useCallback, useState } from "react";

import { Toaster } from "@/components/atoms/ui/sonner";
import { TooltipProvider } from "@/components/atoms/ui/tooltip";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { GlobalSearchDialog } from "@/components/organisms/GlobalSearchDialog";
import { Sidebar } from "@/components/organisms/Sidebar";
import { AppShell } from "@/components/templates/AppShell";
import { useNoteFileIntake } from "@/hooks/useNoteFileIntake";
import { useGlobalSearchShortcut } from "@/hooks/useGlobalSearchShortcut";
import { useSettingsShortcut } from "@/hooks/useSettingsShortcut";
import { useTheme } from "@/hooks/useTheme";
import { GraphPage } from "@/pages/GraphPage";
import { HomePage } from "@/pages/HomePage";
import { NotesPage } from "@/pages/NotesPage";
import { OnboardingPage } from "@/pages/OnboardingPage";
import { SettingsDialog } from "@/pages/SettingsDialog";
import { useHomeState, useNoteCategories, useSetupState } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

export function App() {
  const { data: setup, isPending } = useSetupState();
  const { data: home } = useHomeState();
  const { data: categoryData } = useNoteCategories();
  const view = useSession((s) => s.view);
  const selectedId = useSession((s) => s.selectedId);
  const [searchOpen, setSearchOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const openSearch = useCallback(() => {
    setSettingsOpen(false);
    setSearchOpen(true);
  }, []);
  const openSettings = useCallback(() => {
    setSearchOpen(false);
    setSettingsOpen(true);
  }, []);

  // 選んだテーマ(システム/ライト/ダーク)を <html data-theme> へ反映する
  const theme = useTheme();

  // ノートを開いている間だけ、ペースト・ドロップを添付として受ける
  useNoteFileIntake(selectedId, view === "notes");
  useGlobalSearchShortcut(openSearch, !isPending && !setup?.needs_onboarding);
  useSettingsShortcut(openSettings, !isPending && !setup?.needs_onboarding);

  if (isPending) return null;
  if (setup?.needs_onboarding) return <OnboardingPage />;

  return (
    <TooltipProvider delayDuration={200}>
      <AppShell
        banner={
          <DegradedBanner items={[...(home?.degraded ?? []), ...(categoryData?.degraded ?? [])]} />
        }
        sidebar={
          <Sidebar
            vaultName={setup?.vault_name ?? "kb"}
            categories={categoryData?.categories ?? []}
            onOpenSearch={openSearch}
            settingsOpen={settingsOpen}
            onOpenSettings={openSettings}
          />
        }
      >
        {view === "home" && <HomePage />}
        {view === "notes" && <NotesPage onOpenSearch={openSearch} />}
        {view === "graph" && <GraphPage />}
      </AppShell>

      <GlobalSearchDialog open={searchOpen} onOpenChange={setSearchOpen} />
      <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} />
      <Toaster theme={theme} />
    </TooltipProvider>
  );
}
