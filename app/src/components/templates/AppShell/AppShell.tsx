export interface AppShellProps {
  banner?: React.ReactNode;
  sidebar: React.ReactNode;
  children: React.ReactNode;
}

/** アプリの骨格(告知バー + サイドナビ + 本体)。配置だけを持つ。 */
export function AppShell({ banner, sidebar, children }: AppShellProps) {
  return (
    <div className="flex h-full flex-col">
      {banner}
      <div className="flex min-h-0 flex-1">
        {sidebar}
        {children}
      </div>
    </div>
  );
}
