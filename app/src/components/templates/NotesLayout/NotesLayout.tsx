export interface NotesLayoutProps {
  list?: React.ReactNode;
  listSplitter?: React.ReactNode;
  related?: React.ReactNode;
  relatedSplitter?: React.ReactNode;
  children: React.ReactNode;
}

/**
 * ノート画面だけの多ペイン配置。
 * 表示幅に収まるペインだけを呼び側が渡す。横スクロールで隠れた列を残すと
 * 小さな PC で本文が読めなくなるため、ここでははみ出しを許さない。
 */
export function NotesLayout({
  list,
  listSplitter,
  related,
  relatedSplitter,
  children,
}: NotesLayoutProps) {
  return (
    <div className="flex min-w-0 flex-1 overflow-hidden">
      {list}
      {listSplitter}
      {related}
      {relatedSplitter}
      <div className="flex min-w-0 flex-1 overflow-hidden">{children}</div>
    </div>
  );
}
