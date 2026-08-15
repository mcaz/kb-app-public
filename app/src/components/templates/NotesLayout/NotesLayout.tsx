export interface NotesLayoutProps {
  children: React.ReactNode;
}

/** サイドバーの右側を、一覧または本文の単一主領域として使う。 */
export function NotesLayout({ children }: NotesLayoutProps) {
  return (
    <div className="flex min-w-0 flex-1 overflow-hidden">
      <div className="flex min-w-0 flex-1 overflow-hidden">{children}</div>
    </div>
  );
}
