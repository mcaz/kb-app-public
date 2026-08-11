import { Search } from "lucide-react";

export interface SearchBoxProps {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
}

/** 一覧上部の検索欄。 */
export function SearchBox({ value, onChange, placeholder }: SearchBoxProps) {
  return (
    <div className="relative mx-2.5 mb-1.5 flex">
      <Search className="text-muted pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2" />
      <input
        type="search"
        className="border-line bg-chip text-ink w-full rounded-md border py-1.5 pr-2.5 pl-8 text-[12.5px]"
        value={value}
        placeholder={placeholder}
        aria-label={placeholder}
        onChange={(e) => onChange(e.target.value)}
      />
    </div>
  );
}
