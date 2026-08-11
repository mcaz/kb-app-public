export interface OnboardingLayoutProps {
  /** ブランドマーク(アイコン要素をそのまま受ける)。 */
  mark: React.ReactNode;
  title: string;
  lead: string;
  children: React.ReactNode;
}

export function OnboardingLayout({ mark, title, lead, children }: OnboardingLayoutProps) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-1.5 p-6 text-center">
      <div className="text-grow">{mark}</div>
      <h1 className="mt-1.5 mb-0.5 text-[22px]">{title}</h1>
      <p className="text-muted mt-0 mb-4">{lead}</p>
      <div>{children}</div>
    </div>
  );
}
