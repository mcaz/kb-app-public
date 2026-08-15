import { useCallback, useState } from "react";

import { Toaster } from "@/components/atoms/ui/sonner";
import { TooltipProvider } from "@/components/atoms/ui/tooltip";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { FavoritesDialog } from "@/components/organisms/FavoritesDialog";
import { GlobalSearchDialog } from "@/components/organisms/GlobalSearchDialog";
import { Sidebar } from "@/components/organisms/Sidebar";
import { AppShell } from "@/components/templates/AppShell";
import { useNoteFileIntake } from "@/hooks/useNoteFileIntake";
import { useGlobalSearchShortcut } from "@/hooks/useGlobalSearchShortcut";
import { useTheme } from "@/hooks/useTheme";
import { ConnectPage } from "@/pages/ConnectPage";
import { GraphPage } from "@/pages/GraphPage";
import { HomePage } from "@/pages/HomePage";
import { NotesPage } from "@/pages/NotesPage";
import { OnboardingPage } from "@/pages/OnboardingPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { useHomeState, useSetupState } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

export function App() {
  const { data: setup, isPending } = useSetupState();
  const { data: home } = useHomeState();
  const view = useSession((s) => s.view);
  const selectedId = useSession((s) => s.selectedId);
  const [favoritesOpen, setFavoritesOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const openSearch = useCallback(() => setSearchOpen(true), []);

  // 選んだテーマ(システム/ライト/ダーク)を <html data-theme> へ反映する
  const theme = useTheme();

  // ノートを開いている間だけ、ペースト・ドロップを添付として受ける
  useNoteFileIntake(selectedId, view === "notes");
  useGlobalSearchShortcut(openSearch, !isPending && !setup?.needs_onboarding);

  if (isPending) return null;
  if (setup?.needs_onboarding) return <OnboardingPage />;

  return (
    <TooltipProvider delayDuration={200}>
      <AppShell
        banner={<DegradedBanner messages={home?.degraded ?? []} />}
        sidebar={
          <Sidebar
            vaultName={setup?.vault_name ?? "kb"}
            onOpenSearch={openSearch}
            onOpenFavorites={() => setFavoritesOpen(true)}
          />
        }
      >
        {view === "home" && <HomePage />}
        {view === "notes" && <NotesPage onOpenSearch={openSearch} />}
        {view === "graph" && <GraphPage />}
        {view === "connect" && <ConnectPage />}
        {view === "settings" && <SettingsPage />}
      </AppShell>

      <FavoritesDialog open={favoritesOpen} onOpenChange={setFavoritesOpen} />
      <GlobalSearchDialog open={searchOpen} onOpenChange={setSearchOpen} />
      <Toaster theme={theme} />
    </TooltipProvider>
  );
}
