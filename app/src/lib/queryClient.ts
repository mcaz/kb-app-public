import { QueryClient, type DefaultOptions } from "@tanstack/react-query";

/**
 * 通常の画面queryはフォーカス復帰で一斉再取得しない。
 * 外部更新の確認はmaintenance queryだけが担当する。
 */
export const queryDefaults: DefaultOptions = {
  queries: {
    retry: false,
    refetchOnWindowFocus: false,
    staleTime: 5_000,
  },
};

export const createQueryClient = () => new QueryClient({ defaultOptions: queryDefaults });
