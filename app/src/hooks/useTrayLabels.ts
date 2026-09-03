import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { api } from "@/lib/api";

/**
 * trayメニューを画面と同じ言語にする。
 *
 * 言語設定はwebview側にしかないので、native側は既定の日本語で常駐を始め、
 * 画面が立ち上がった時点と言語切り替えのたびにここで渡し直す。
 * trayが無い環境(ブラウザプレビュー)では api 側が何もしない。
 */
export function useTrayLabels() {
  const { t, i18n } = useTranslation("common");

  useEffect(() => {
    void api.traySetLabels(t("tray.show"), t("tray.quit")).catch(() => {
      // trayの文言が古いままでも常駐と復帰は動く。画面を止める理由にしない。
    });
  }, [t, i18n.language]);
}
