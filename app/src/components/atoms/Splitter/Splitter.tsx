import { useRef } from "react";

export interface SplitterProps {
  /** いまの幅(px)。ドラッグ中もこの値を更新して呼び出し側が反映する。 */
  width: number;
  min: number;
  max: number;
  onChange: (width: number) => void;
  /** ドラッグ終了時(この時点で永続化すると書き込みが1回で済む)。 */
  onCommit?: (width: number) => void;
  label: string;
}

/**
 * パネル幅のドラッグ調整。旧実装は対象要素の style を直接書き換えていたが、
 * ここでは値だけを返して描画は呼び出し側に任せる(状態の出所を1つにする)。
 * キーボードでも動かせる(← →、Shift で粗く)。
 */
export function Splitter({ width, min, max, onChange, onCommit, label }: SplitterProps) {
  const drag = useRef<{ startX: number; startWidth: number } | null>(null);
  const clamp = (w: number) => Math.min(max, Math.max(min, w));

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { startX: e.clientX, startWidth: width };
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    onChange(clamp(drag.current.startWidth + e.clientX - drag.current.startX));
  };

  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    const next = clamp(drag.current.startWidth + e.clientX - drag.current.startX);
    drag.current = null;
    onCommit?.(next);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 32 : 8;
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const next = clamp(width + (e.key === "ArrowRight" ? step : -step));
    onChange(next);
    onCommit?.(next);
  };

  return (
    // WAI-ARIA APG の Window Splitter パターン。role="separator" にフォーカスと
    // 矢印キー操作を持たせるのが正しい形だが、jsx-a11y はこの例外を知らないため
    // ここだけ無効化する(https://www.w3.org/WAI/ARIA/apg/patterns/windowsplitter/)
    // eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={width}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      className="hover:bg-grow-soft focus-visible:bg-grow-soft w-[5px] flex-none cursor-col-resize bg-transparent"
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onKeyDown={handleKeyDown}
    />
  );
}
