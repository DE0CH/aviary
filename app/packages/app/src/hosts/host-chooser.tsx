import { useCallback } from "react";
import { router } from "expo-router";
import { create } from "zustand";
import { usePanelStore } from "@/stores/panel-store";
import { useLocalDaemonServerId } from "@/hooks/use-is-local-daemon";
import { useHosts } from "@/runtime/host-runtime";
import { orderHostsLocalFirst, type HostProfile } from "@/types/host-connection";
import { buildSettingsAddHostRoute } from "@/utils/host-routes";

// The host chooser's request store and hook; the modal that shows a request is hosts/host-chooser-modal.tsx, mounted
// with the hosts chunk (loading/hosts-chunk.tsx), so a request made before it arrives shows once it has.

type HostFilter = (host: HostProfile) => boolean;
type HostChoiceHandler = (serverId: string) => void | Promise<void>;

export interface ChooseHostInput {
  title?: string;
  filter?: HostFilter;
  onChooseHost: HostChoiceHandler;
  onNoHosts?: () => void;
}

export interface HostChoiceRequest {
  id: number;
  title: string;
  serverIds: string[];
  onChooseHost: HostChoiceHandler;
}

interface HostChooserState {
  request: HostChoiceRequest | null;
  open: (request: Omit<HostChoiceRequest, "id">) => void;
  close: () => void;
}

let nextRequestId = 1;

export const useHostChooserStore = create<HostChooserState>((set) => ({
  request: null,
  open: (request) => {
    set({ request: { ...request, id: nextRequestId++ } });
  },
  close: () => set({ request: null }),
}));

export function useHostChooser() {
  const hosts = useHosts();
  const localServerId = useLocalDaemonServerId();
  const open = useHostChooserStore((state) => state.open);

  return useCallback(
    (input: ChooseHostInput) => {
      const availableHosts = orderHostsLocalFirst(hosts, localServerId).filter(
        input.filter ?? (() => true),
      );

      if (availableHosts.length === 0) {
        (
          input.onNoHosts ??
          (() => {
            usePanelStore.getState().showMobileAgent();
            router.push(buildSettingsAddHostRoute(Date.now()));
          })
        )();
        return false;
      }

      if (availableHosts.length === 1) {
        void input.onChooseHost(availableHosts[0].serverId);
        return true;
      }

      open({
        title: input.title ?? "Choose host",
        serverIds: availableHosts.map((host) => host.serverId),
        onChooseHost: input.onChooseHost,
      });
      return true;
    },
    [hosts, localServerId, open],
  );
}

