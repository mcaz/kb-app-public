import { QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/App";
import { setupI18n } from "@/i18n";
import { createQueryClient } from "@/lib/queryClient";
import { usePrefs } from "@/lib/stores/prefs";
import { applyTheme } from "@/lib/theme";
import "@/styles/app.css";

// 最初の描画より前に配色を確定させる(一瞬ライトで出てから切り替わるのを避ける)
applyTheme(usePrefs.getState().theme);
setupI18n(usePrefs.getState().language);

const queryClient = createQueryClient();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
);
