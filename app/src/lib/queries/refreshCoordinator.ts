import { matchQuery, type Query, type QueryClient, type QueryFilters } from "@tanstack/react-query";

type PendingRefresh = { filters: QueryFilters | undefined };

/**
 * 復帰・外部変更の後に始めた取得を保証する。cancelRefetch:falseだけでは、変更前に
 * 始めた応答へ要求が吸収される。IPCを重複起動せず、queryごとに完了後の1回へまとめる。
 */
export function createRefreshCoordinator(client: QueryClient) {
  let disposed = false;
  const pending = new Map<Query, PendingRefresh>();
  const canRefresh = (query: Query, filters: QueryFilters) =>
    !disposed &&
    client.getQueryCache().get(query.queryHash) === query &&
    query.isActive() &&
    !query.isDisabled() &&
    !query.isStatic() &&
    matchQuery(filters, query);

  const drain = async (query: Query, request: PendingRefresh) => {
    try {
      while (request.filters && canRefresh(query, request.filters)) {
        if (query.state.fetchStatus !== "idle") {
          const running = query.promise;
          if (!running) break;
          // 無効化が進行中の取得を置き換えても、新しいPromiseの完了まで待つ。
          await running.catch(() => undefined);
          continue;
        }
        request.filters = undefined;
        await client.refetchQueries(
          { type: "active", predicate: (candidate) => candidate === query },
          { cancelRefetch: false },
        );
      }
    } finally {
      pending.delete(query);
    }
  };

  return {
    refresh(filters: QueryFilters) {
      if (disposed) return;
      for (const query of client.getQueryCache().findAll(filters)) {
        if (!canRefresh(query, filters)) continue;
        const existing = pending.get(query);
        if (existing) existing.filters = filters;
        else {
          const request = { filters };
          pending.set(query, request);
          void drain(query, request);
        }
      }
    },
    dispose() {
      disposed = true;
      pending.clear();
    },
  };
}
