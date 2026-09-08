import { QueryClient, type DefaultOptions } from "@tanstack/react-query";

/**
 * 通常の画面queryはフォーカス復帰で一斉再取得しない。
 * 読み取りの自動更新は明示したqueryだけを対象にし、復帰通知は共通hookで受ける。
 * 同期・export等の保守は別の間隔を守る。
 */
export const queryDefaults: DefaultOptions = {
  queries: {
    retry: false,
    refetchOnWindowFocus: false,
    staleTime: 5_000,
  },
};

export const createQueryClient = () => new QueryClient({ defaultOptions: queryDefaults });
