import { tv } from "tailwind-variants";

/**
 * 一覧の行の枠。選択は cmdk の `data-selected` が持つ。
 *
 * 中身([`NoteResultRow`])と対で使う。枠だけ画面ごとに複製すると、選択枠や
 * 余白を変えたとき片方だけ古いまま残る。
 */
export const noteResultItemVariants = tv({
  base: "flex cursor-pointer flex-col items-stretch gap-1 border border-transparent px-3 py-2.5 data-[selected=true]:border-line",
});
