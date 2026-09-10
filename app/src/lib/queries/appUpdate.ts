import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "@/lib/api";

import { isAppUpdateBusy } from "./appUpdatePolicy";
import { appUpdateMutationKey, queryKeys } from "./keys";

const actions = {
  check: api.appUpdateCheck,
  download: api.appUpdateDownload,
  install: api.appUpdateInstall,
};

export function useAppUpdateStatus(poll = true) {
  const pending = useIsMutating({ mutationKey: appUpdateMutationKey });
  return useQuery({
    queryKey: queryKeys.appUpdate,
    queryFn: api.appUpdateStatus,
    networkMode: "always",
    retry: false,
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    // commandが完了まで応答しない場合も、nativeの進捗を表示できるようにする。
    refetchInterval: (query) =>
      poll && (pending > 0 || isAppUpdateBusy(query.state.data?.phase)) ? 500 : false,
  });
}

export function useAppUpdateAction() {
  const qc = useQueryClient();
  const pending = useIsMutating({ mutationKey: appUpdateMutationKey });
  const mutation = useMutation({
    mutationKey: appUpdateMutationKey,
    mutationFn: (action: keyof typeof actions) => actions[action](),
    networkMode: "always",
    retry: false,
    onMutate: () => qc.cancelQueries({ queryKey: queryKeys.appUpdate }),
    onSuccess: async (status) => {
      // 応答前に始まったpollが、確定済みの状態を古い進捗で上書きしない。
      await qc.cancelQueries({ queryKey: queryKeys.appUpdate });
      qc.setQueryData(queryKeys.appUpdate, status);
    },
    onSettled: () => qc.invalidateQueries({ queryKey: queryKeys.appUpdate }),
  });
  return { ...mutation, isPending: mutation.isPending || pending > 0 };
}
