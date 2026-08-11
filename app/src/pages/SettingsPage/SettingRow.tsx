export interface SettingRowProps {
  id: string;
  label: string;
  children: React.ReactNode;
}

/** SettingsPage 私物: ラベルと操作を1行に並べる。 */
export function SettingRow({ id, label, children }: SettingRowProps) {
  return (
    <div className="flex items-center justify-between gap-4">
      <label htmlFor={id} className="text-[13px]">
        {label}
      </label>
      {children}
    </div>
  );
}
