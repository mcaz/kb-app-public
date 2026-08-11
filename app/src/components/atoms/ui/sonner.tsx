import { Toaster as Sonner, type ToasterProps } from "sonner";

/**
 * トースト。配色はアプリのテーマ(設定 > 見た目)に従う。
 * 実際の色はトークン変数から引くので theme prop は補助的な指定。
 * 見た目は旧 .toast(下中央・角丸・ink 地)に合わせる。
 */
const Toaster = (props: ToasterProps) => (
  <Sonner
    position="bottom-center"
    toastOptions={{
      style: {
        background: "var(--color-ink)",
        color: "var(--color-ground)",
        border: "none",
        borderRadius: "999px",
        fontSize: "13px",
      },
    }}
    {...props}
  />
);

export { Toaster };
