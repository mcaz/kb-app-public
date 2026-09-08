/**
 * MCPなど別プロセスの変更を拾い、非表示のqueryはTanStack Queryに停止させる。
 * ローカルIPCの読み取りは、ブラウザのオフライン判定でも止めない。
 */
export const liveQueryOptions = {
  refetchInterval: 15_000,
  networkMode: "always",
  meta: { refreshOnResume: true },
} as const;
