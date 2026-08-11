/** 表示用の整形。ロケールを引数で受け、i18n の現在言語に追従させる。 */

const dayCache = new Map<string, Intl.DateTimeFormat>();
const cached = (key: string, make: () => Intl.DateTimeFormat) => {
  let f = dayCache.get(key);
  if (!f) {
    f = make();
    dayCache.set(key, f);
  }
  return f;
};

/** 一覧の日付(同じ年なら年を省く)。 */
export function formatDay(iso: string | null, locale: string, now = new Date()): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  const sameYear = d.getFullYear() === now.getFullYear();
  const key = `day:${locale}:${String(sameYear)}`;
  return cached(
    key,
    () =>
      new Intl.DateTimeFormat(locale, {
        year: sameYear ? undefined : "numeric",
        month: "numeric",
        day: "numeric",
      }),
  ).format(d);
}

/** 本文ヘッダの日時(秒まで)。 */
export function formatDateTime(iso: string | null, locale: string, unknown = "不明"): string {
  if (!iso) return unknown;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return unknown;
  return cached(
    `dt:${locale}`,
    () => new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "medium" }),
  ).format(d);
}

/** 添付のサイズ。 */
export function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)}MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)}KB`;
  return `${bytes}B`;
}
