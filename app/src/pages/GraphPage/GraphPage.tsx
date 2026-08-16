import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { GraphCanvas } from "@/components/organisms/GraphCanvas";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { subgraph } from "@/lib/graph/subgraph";
import { useGraphData } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

/** つながりグラフ。中心が指定されていれば2ホップの局所グラフにする。 */
export function GraphPage() {
  const { t } = useTranslation("graph");
  const { data } = useGraphData();
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
      {shown && <GraphCanvas data={shown} centerId={focus} hint={t("hint")} onOpen={openNote} />}
    </SinglePaneLayout>
  );
}
