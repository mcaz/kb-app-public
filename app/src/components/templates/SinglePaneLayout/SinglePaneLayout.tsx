export interface SinglePaneLayoutProps {
  children: React.ReactNode;
  scroll?: boolean;
}

/** ホーム・グラフ・繋ぐで使う単一ペイン。 */
export function SinglePaneLayout({ children, scroll = true }: SinglePaneLayoutProps) {
  return (
    <div
      className={`flex min-w-0 flex-1 flex-col ${scroll ? "overflow-y-auto" : "overflow-hidden"}`}
    >
      {children}
    </div>
  );
}
