import { useTranslation } from "react-i18next";

import { Splitter } from "@/components/atoms/Splitter";
import { NoteListPanel } from "@/components/organisms/NoteListPanel";
import { NotePane } from "@/components/organisms/NotePane";
import { RelatedPanel } from "@/components/organisms/RelatedPanel";
import { NotesLayout } from "@/components/templates/NotesLayout";
import { usePrefs } from "@/lib/stores/prefs";
import { useSession } from "@/lib/stores/session";

/** ノート画面(一覧 + 関連 + 本文)。この製品の中心。 */
export function NotesPage() {
  const { t } = useTranslation("notes");
  const selectedId = useSession((s) => s.selectedId);
  const secondaryId = useSession((s) => s.secondaryId);
  const prefs = usePrefs();

  return (
    <NotesLayout
      list={<NoteListPanel width={prefs.listWidth} />}
      listSplitter={
        <Splitter
          label={t("search.placeholder")}
          width={prefs.listWidth}
          min={180}
          max={520}
          onChange={(listWidth) => prefs.set({ listWidth })}
        />
      }
      related={selectedId ? <RelatedPanel noteId={selectedId} width={prefs.relWidth} /> : undefined}
      relatedSplitter={
        selectedId ? (
          <Splitter
            label={t("related.linked")}
            width={prefs.relWidth}
            min={160}
            max={420}
            onChange={(relWidth) => prefs.set({ relWidth })}
          />
        ) : undefined
      }
    >
      {selectedId ? (
        <>
          <div
            className="flex min-w-0 flex-1"
            style={
              secondaryId && prefs.mainWidth ? { flex: "none", width: prefs.mainWidth } : undefined
            }
          >
            <NotePane noteId={selectedId} />
          </div>
          {secondaryId && (
            <>
              <Splitter
                label={t("selection.secondary")}
                width={prefs.mainWidth ?? 640}
                min={320}
                max={1400}
                onChange={(mainWidth) => prefs.set({ mainWidth })}
              />
              <NotePane noteId={secondaryId} secondary />
            </>
          )}
        </>
      ) : (
        <p className="text-muted p-[18px]">{t("list.placeholder")}</p>
      )}
    </NotesLayout>
  );
}
