/** 表示用の整形。日時は全画面で同じ固定形式にする。 */

const dayCache = new Map<string, Intl.DateTimeFormat>();
const cached = (key: string, make: () => Intl.DateTimeFormat) => {
  let f = dayCache.get(key);
  if (!f) {
    f = make();
    dayCache.set(key, f);
  }
  return f;
};

/** ローカル時刻を `yyyy/MM/dd HH:mm:ss` で返す。 */
export function formatDateTime(iso: string | null, locale: string, unknown = "不明"): string {
  if (!iso) return unknown;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return unknown;
  const parts = cached(
    `dt:${locale}`,
    () =>
      new Intl.DateTimeFormat(locale, {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hourCycle: "h23",
      }),
  ).formatToParts(d);
  const value = Object.fromEntries(parts.map((part) => [part.type, part.value]));
  return `${value.year}/${value.month}/${value.day} ${value.hour}:${value.minute}:${value.second}`;
}

/** 添付のサイズ。 */
export function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)}MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)}KB`;
  return `${bytes}B`;
}
