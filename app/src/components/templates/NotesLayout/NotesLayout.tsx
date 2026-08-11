export interface NotesLayoutProps {
  list: React.ReactNode;
  listSplitter: React.ReactNode;
  related?: React.ReactNode;
  relatedSplitter?: React.ReactNode;
  children: React.ReactNode;
}

/**
 * ノート画面だけの多ペイン配置。
 * パネルを跨いで左右に動かせるよう、ここだけ横スクロールを許す
 * (他の画面は単一ペインなので、この指定を共有すると幅0に潰れる — 旧実装の事故)。
 */
export function NotesLayout({
  list,
  listSplitter,
  related,
  relatedSplitter,
  children,
}: NotesLayoutProps) {
  return (
    <div className="flex min-w-0 flex-1 overflow-x-auto overflow-y-hidden [&>*]:flex-none">
      {list}
      {listSplitter}
      {related}
      {relatedSplitter}
      <div className="flex min-w-[420px] flex-1 shrink-0 grow overflow-hidden">{children}</div>
    </div>
  );
}
