export interface NotesLayoutProps {
  browser?: React.ReactNode;
  children: React.ReactNode;
}

/**
 * ノート画面だけの多ペイン配置。
 * 表示幅に収まるペインだけを呼び側が渡す。横スクロールで隠れた列を残すと
 * 小さな PC で本文が読めなくなるため、ここでははみ出しを許さない。
 */
export function NotesLayout({ browser, children }: NotesLayoutProps) {
  return (
    <div className="flex min-w-0 flex-1 overflow-hidden">
      {browser}
      <div className="flex min-w-0 flex-1 overflow-hidden">{children}</div>
    </div>
  );
}
