import { Link, Sparkles, Waypoints } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";

import { RelatedList } from "@/components/molecules/RelatedList";
import { Button } from "@/components/atoms/ui/button";
import { useNote } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

export interface RelatedPanelProps {
  noteId: string;
  width: number;
}

/** ノート一覧の右のカラム。開いているノートのリンク・近いノート・グラフ入口。 */
export function RelatedPanel({ noteId, width }: RelatedPanelProps) {
  const { t } = useTranslation("notes");
  const { data: note } = useNote(noteId);
  const secondaryId = useSession((s) => s.secondaryId);
  const openBeside = useSession((s) => s.openBeside);
  const focusGraph = useSession((s) => s.focusGraph);

  if (!note) return <aside style={{ width }} className="bg-panel-2 flex-none" />;

  return (
    <aside
      style={{ width }}
      className="bg-panel-2 flex flex-none flex-col overflow-y-auto px-2.5 py-3 max-[1040px]:hidden"
    >
      <Button className="mb-2.5 w-full justify-start" onClick={() => focusGraph(note.id)}>
        <Icon as={Waypoints} size="sm" />
        {t("related.openGraph")}
      </Button>
      <div className="flex flex-1 flex-col gap-1 overflow-y-auto">
        <RelatedList
          head={t("related.linked")}
          headIcon={Link}
          tone="linked"
          emptyLabel={t("related.linkedEmpty")}
          openId={secondaryId}
          onOpen={openBeside}
          entries={note.related.map(([id, title]) => ({ id, title: title ?? id }))}
        />
        <div className="mt-3" />
        <RelatedList
          head={t("related.similar")}
          headIcon={Sparkles}
          tone="similar"
          emptyLabel={t("related.similarEmpty")}
          openId={secondaryId}
          onOpen={openBeside}
          entries={note.similar.map(([id, title, distance]) => ({
            id,
            title: title ?? id,
            distance,
          }))}
        />
      </div>
    </aside>
  );
}
