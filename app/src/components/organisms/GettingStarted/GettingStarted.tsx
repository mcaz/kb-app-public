import { Copy, Plug, Search, Shield } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/atoms/ui/button";
import { useSetupState } from "@/lib/queries";

import { gettingStartedVariants } from "./variants";

interface GettingStartedProps {
  onOpenConnections: () => void;
  onOpenProtection: () => void;
  onOpenSearch: () => void;
}

/** 登録やコピーは実操作の証拠にならないため、達成状態を持たない使い方ガイドにする。 */
export function GettingStarted({
  onOpenConnections,
  onOpenProtection,
  onOpenSearch,
}: GettingStartedProps) {
  const { t } = useTranslation("gettingStarted");
  const setup = useSetupState();
  const styles = gettingStartedVariants();
  const name = setup.data?.vault_name ?? t("selectedKnowledgeBase");
  const recordPrompt = t("record.prompt", { name });
  const searchPrompt = t("search.prompt", { name });

  const copy = async (prompt: string) => {
    try {
      await navigator.clipboard.writeText(prompt);
      toast(t("copied"));
    } catch {
      toast(t("copyFailed"));
    }
  };

  return (
    <section className={styles.section()} aria-labelledby="getting-started-title">
      <h1 id="getting-started-title" className={styles.title()}>
        {t("title")}
      </h1>
      <p className={styles.lead()}>{t("lead", { name })}</p>
      <ol className={styles.steps()}>
        <li className={styles.step()}>
          <span className={styles.number()} aria-hidden="true">
            1
          </span>
          <div className={styles.content()}>
            <h2 className={styles.heading()}>{t("connect.title")}</h2>
            <p className={styles.description()}>{t("connect.description")}</p>
            <div className={styles.actions()}>
              <Button variant="primary" size="sm" onClick={onOpenConnections}>
                <Plug className={styles.icon()} />
                {t("connect.open")}
              </Button>
              <Button size="sm" onClick={onOpenProtection}>
                <Shield className={styles.icon()} />
                {t("connect.protection")}
              </Button>
            </div>
            <p className={styles.hint()}>{t("connect.reconnect")}</p>
          </div>
        </li>
        <li className={styles.step()}>
          <span className={styles.number()} aria-hidden="true">
            2
          </span>
          <div className={styles.content()}>
            <h2 className={styles.heading()}>{t("record.title")}</h2>
            <p className={styles.description()}>{t("record.description")}</p>
            <p className={styles.prompt()}>{recordPrompt}</p>
            <Button size="sm" onClick={() => void copy(recordPrompt)}>
              <Copy className={styles.icon()} />
              {t("record.copy")}
            </Button>
            <p className={styles.hint()}>{t("record.check")}</p>
          </div>
        </li>
        <li className={styles.step()}>
          <span className={styles.number()} aria-hidden="true">
            3
          </span>
          <div className={styles.content()}>
            <h2 className={styles.heading()}>{t("search.title")}</h2>
            <p className={styles.description()}>{t("search.description")}</p>
            <p className={styles.prompt()}>{searchPrompt}</p>
            <div className={styles.actions()}>
              <Button size="sm" onClick={() => void copy(searchPrompt)}>
                <Copy className={styles.icon()} />
                {t("search.copy")}
              </Button>
              <Button size="sm" onClick={onOpenSearch}>
                <Search className={styles.icon()} />
                {t("search.open")}
              </Button>
            </div>
            <p className={styles.hint()}>{t("search.check")}</p>
          </div>
        </li>
      </ol>
      <p className={styles.footnote()}>{t("evidence")}</p>
      <p className={styles.hint()}>{t("returnHint")}</p>
    </section>
  );
}
