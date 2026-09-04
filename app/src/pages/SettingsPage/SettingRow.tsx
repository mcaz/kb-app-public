export interface SettingRowProps {
  id: string;
  label: string;
  /** 操作の意味が名前だけで伝わらない行に添える(自動起動など)。 */
  description?: string;
  children: React.ReactNode;
}

/** SettingsPage 私物: ラベルと操作を1行に並べる。 */
export function SettingRow({ id, label, description, children }: SettingRowProps) {
  const descriptionId = `${id}-description`;
  return (
    <div className="flex items-center justify-between gap-4">
      <div className="min-w-0">
        <label htmlFor={id} className="text-[13px]">
          {label}
        </label>
        {description && (
          <p id={descriptionId} className="text-muted mt-1 text-xs leading-relaxed">
            {description}
          </p>
        )}
      </div>
      {children}
    </div>
  );
}
