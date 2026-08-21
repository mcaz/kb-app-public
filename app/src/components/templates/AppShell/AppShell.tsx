export interface AppShellProps {
  banner?: React.ReactNode;
  tabs: React.ReactNode;
  sidebar: React.ReactNode;
  children: React.ReactNode;
}

/** アプリの骨格(告知バー + タブ + サイドナビ + 本体)。配置だけを持つ。 */
export function AppShell({ banner, tabs, sidebar, children }: AppShellProps) {
  return (
    <div className="bg-ground flex h-full flex-col overflow-hidden">
      {banner}
      {tabs}
      <div className="flex min-h-0 flex-1">
        {sidebar}
        <div
          id="workspace-panel"
          role="tabpanel"
          className="bg-panel min-w-0 flex-1 overflow-hidden"
        >
          {children}
        </div>
      </div>
    </div>
  );
}
