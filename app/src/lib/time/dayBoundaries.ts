/** 固定の24時間を引くと夏時間の切り替え日に日付がずれるため、暦日から境界を作る。 */
export function localDayBoundaries(now = new Date()): number[] {
  return Array.from({ length: 15 }, (_, index) =>
    new Date(now.getFullYear(), now.getMonth(), now.getDate() - 13 + index).getTime(),
  );
}
