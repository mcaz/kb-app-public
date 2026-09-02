import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { GraphCanvas } from "@/components/organisms/GraphCanvas";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useErrorText } from "@/hooks/useErrorText";
import { subgraph } from "@/lib/graph/subgraph";
import { useGraphData } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

/** つながりグラフ。中心が指定されていれば2ホップの局所グラフにする。 */
export function GraphPage() {
  const { t } = useTranslation("graph");
  const query = useGraphData();
  const { data } = query;
  const errorText = useErrorText();
  const focus = useSession((s) => s.graphFocus);
  const focusGraph = useSession((s) => s.focusGraph);
  const openNote = useSession((s) => s.openNote);

  const shown = useMemo(() => {
    if (!data) return null;
    return focus ? subgraph(data, focus, 2) : data;
  }, [data, focus]);

  const centerTitle = data?.nodes.find((n) => n.id === focus)?.title ?? focus;

  return (
    <SinglePaneLayout scroll={false}>
      <DegradedBanner items={data?.degraded ?? []} />
      {focus && (
        <div className="border-line text-muted flex items-center justify-between gap-2.5 border-b px-3.5 py-2 text-[12.5px]">
          <span>{t("around", { title: centerTitle })}</span>
          <Button variant="quiet" size="sm" onClick={() => focusGraph(null)}>
            {t("showAll")}
          </Button>
        </div>
      )}
      {/* 取得できていないときに何も描かないと、失敗が利用者へ届かないまま無反応に見える。 */}
      {query.isPending ? (
        <p className="text-muted m-0 px-3.5 py-3 text-xs" role="status">
          {t("loading")}
        </p>
      ) : query.isError ? (
        <div className="px-3.5 py-3 text-xs" role="alert">
          <p className="text-muted m-0 mb-2">{errorText(query.error)}</p>
          <Button size="sm" onClick={() => void query.refetch()}>
            {t("retry")}
          </Button>
        </div>
      ) : shown && shown.nodes.length === 0 ? (
        <p className="text-muted m-0 px-3.5 py-3 text-xs">{t("empty")}</p>
      ) : (
        shown && <GraphCanvas data={shown} centerId={focus} hint={t("hint")} onOpen={openNote} />
      )}
    </SinglePaneLayout>
  );
}
