export interface SelectionEntry {
  id: string;
  title: string;
  secondary: boolean;
}

export interface SelectionStripProps {
  entries: SelectionEntry[];
  labels: { head: string; primary: string; secondary: string; close: string };
  onFocus: (entry: SelectionEntry) => void;
  onClose: () => void;
}

/** 開いているノート(絞り込みで一覧から消えても、ここには残る)。 */
export function SelectionStrip({ entries, labels, onFocus, onClose }: SelectionStripProps) {
  if (entries.length === 0) return null;
  return (
    <div className="border-line flex-none border-b px-2.5 pb-2">
      <div className="text-muted mb-1 text-[10px] tracking-[0.1em]">{labels.head}</div>
      {entries.map((entry) => (
        <div
          key={entry.id}
          className={`mb-0.5 flex items-center gap-1.5 rounded-md border px-2 py-1 text-xs ${
            entry.secondary ? "border-line bg-panel" : "border-grow bg-sel"
          }`}
        >
          <span
            className={`flex-none rounded border px-1 text-[9.5px] leading-[14px] ${
              entry.secondary ? "border-line text-muted" : "border-grow text-grow"
            }`}
          >
            {entry.secondary ? labels.secondary : labels.primary}
          </span>
          <button
            type="button"
            className="min-w-0 flex-1 cursor-pointer truncate border-none bg-transparent text-left text-inherit"
            onClick={() => onFocus(entry)}
          >
            {entry.title}
          </button>
          {entry.secondary && (
            <button
              type="button"
              className="text-muted hover:text-danger cursor-pointer border-none bg-transparent px-0.5 text-xs"
              aria-label={labels.close}
              onClick={onClose}
            >
              ×
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
