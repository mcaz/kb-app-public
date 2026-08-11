import { useRef, useState } from "react";

import { TagChip } from "@/components/atoms/TagChip";
import { Button } from "@/components/atoms/ui/button";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/atoms/ui/command";

export interface TagComboboxProps {
  allTags: string[];
  selected: string[];
  onAdd: (tag: string) => void;
  onRemove: (tag: string) => void;
  onClear: () => void;
  labels: { placeholder: string; clear: string; clearTitle: string; empty: string };
}

/**
 * タグの複数選択(AND 絞り込み)。
 *
 * 旧実装は1つ選ぶたびに画面全体を作り直していたため入力欄のフォーカスが飛び、
 * 続けてタグを打てなかった。ここでは入力欄を保ったまま候補だけ差し替える。
 *
 * 候補一覧は Popover(ポータル)に載せない。cmdk は Command の DOM 部分木に
 * 項目があることを前提に上下キー・Enter を処理するため、ポータルへ出すと
 * キーボードで選べなくなる。旧実装と同じく入力欄の直下に重ねる。
 */
export function TagCombobox({
  allTags,
  selected,
  onAdd,
  onRemove,
  onClear,
  labels,
}: TagComboboxProps) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const candidates = allTags.filter((t) => !selected.includes(t));

  const add = (tag: string) => {
    onAdd(tag);
    setQuery("");
    // 選んだ後もそのまま次のタグを打てるようにする
    inputRef.current?.focus();
  };

  return (
    <div className="relative mx-2.5 mb-2.5 flex flex-wrap items-center gap-1">
      {selected.map((tag) => (
        <TagChip key={tag} tag={tag} selected onRemove={() => onRemove(tag)} />
      ))}

      <Command
        shouldFilter
        className="relative min-w-[90px] flex-1 overflow-visible bg-transparent"
      >
        <CommandInput
          ref={inputRef}
          value={query}
          onValueChange={setQuery}
          onFocus={() => setOpen(true)}
          onClick={() => setOpen(true)}
          // 候補をクリックし終える前に閉じないよう、少し待ってから畳む
          onBlur={() => setTimeout(() => setOpen(false), 120)}
          onKeyDown={(e) => {
            if (e.key === "Escape") setOpen(false);
            // 入力が空のまま Backspace = 末尾のタグを外す(旧実装の挙動)
            if (e.key === "Backspace" && query === "" && selected.length > 0) {
              const last = selected[selected.length - 1];
              if (last) onRemove(last);
            }
          }}
          placeholder={selected.length ? "" : labels.placeholder}
          className="h-auto border-none bg-transparent px-1 py-0.5 text-xs"
        />
        {open && candidates.length > 0 && (
          <CommandList className="border-line bg-panel absolute top-full left-0 z-20 mt-1 min-w-[180px] rounded-lg border shadow-lg">
            <CommandEmpty>{labels.empty}</CommandEmpty>
            <CommandGroup>
              {candidates.map((tag) => (
                <CommandItem key={tag} value={tag} onSelect={() => add(tag)}>
                  {tag}
                </CommandItem>
              ))}
            </CommandGroup>
          </CommandList>
        )}
      </Command>

      {selected.length >= 2 && (
        <Button size="sm" title={labels.clearTitle} onClick={onClear}>
          {labels.clear}
        </Button>
      )}
    </div>
  );
}
