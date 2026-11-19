import { createElement, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Modal,
  Pressable,
  ScrollView,
  Text,
  View,
  type PressableStateCallbackType,
} from "react-native";
import {
  EditingTextInput as TextInput,
  type EditingTextInputHandle,
} from "@/components/ui/text-input";
import { Server } from "lucide-react-native";
import { StyleSheet, useUnistyles } from "react-native-unistyles";
import { HostStatusDotSlot } from "@/components/hosts/host-picker";
import { isWeb } from "@/constants/platform";
import {
  OverlayLayerProvider,
  useGlobalWebOverlayLayer,
  useWebOverlayRegistration,
} from "@/lib/overlay-root";
import { useHosts } from "@/runtime/host-runtime";
import type { HostProfile } from "@/types/host-connection";

import { useHostChooserStore } from "./host-chooser";

function matchesHostQuery(host: HostProfile, query: string): boolean {
  const normalized = query.trim().toLowerCase();
  if (!normalized) return true;
  return (
    host.label.toLowerCase().includes(normalized) ||
    host.serverId.toLowerCase().includes(normalized)
  );
}

function HostChooserRow({
  host,
  active,
  onChooseHost,
}: {
  host: HostProfile;
  active: boolean;
  onChooseHost: (serverId: string) => void;
}) {
  const { theme } = useUnistyles();
  const handlePress = useCallback(() => onChooseHost(host.serverId), [host.serverId, onChooseHost]);
  const rowStyle = useCallback(
    ({ hovered = false, pressed }: PressableStateCallbackType & { hovered?: boolean }) => [
      styles.row,
      (active || hovered || pressed) && styles.rowActive,
    ],
    [active],
  );

  return (
    <Pressable
      accessibilityRole="button"
      onPress={handlePress}
      style={rowStyle}
      testID={`host-chooser-row-${host.serverId}`}
    >
      <View style={styles.rowIconSlot}>
        <HostStatusDotSlot serverId={host.serverId} />
      </View>
      <View style={styles.rowText}>
        <Text style={styles.rowTitle} numberOfLines={1}>
          {host.label}
        </Text>
        <Text style={styles.rowSubtitle} numberOfLines={1}>
          {host.serverId}
        </Text>
      </View>
      <Server size={theme.iconSize.sm} color={theme.colors.foregroundMuted} />
    </Pressable>
  );
}

export function HostChooserModal() {
  const { theme } = useUnistyles();
  const hosts = useHosts();
  const request = useHostChooserStore((state) => state.request);
  const close = useHostChooserStore((state) => state.close);
  const inputRef = useRef<EditingTextInputHandle>(null);
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const modalLayer = useGlobalWebOverlayLayer("modal", isWeb && request != null);

  const requestHosts = useMemo(() => {
    if (!request) return [];
    const hostByServerId = new Map(hosts.map((host) => [host.serverId, host] as const));
    return request.serverIds.flatMap((serverId) => {
      const host = hostByServerId.get(serverId);
      return host ? [host] : [];
    });
  }, [hosts, request]);

  const options = useMemo(
    () => requestHosts.filter((host) => matchesHostQuery(host, query)),
    [query, requestHosts],
  );
  const activeOptionIndex = options.length === 0 ? 0 : Math.min(activeIndex, options.length - 1);

  useEffect(() => {
    if (!request) return;
    inputRef.current?.replaceText("");
    setQuery("");
    setActiveIndex(0);
    const id = setTimeout(() => inputRef.current?.focus(), 0);
    return () => clearTimeout(id);
  }, [request]);

  const handleQueryChange = useCallback((value: string) => {
    setQuery(value);
    setActiveIndex(0);
  }, []);

  const chooseHost = useCallback(
    (serverId: string) => {
      const currentRequest = request;
      close();
      if (!currentRequest) return;
      void currentRequest.onChooseHost(serverId);
    },
    [close, request],
  );

  const handleWebOverlayKeyDown = useCallback(
    (event: KeyboardEvent) => {
      if (
        event.key !== "ArrowDown" &&
        event.key !== "ArrowUp" &&
        event.key !== "Enter" &&
        event.key !== "Escape"
      ) {
        return false;
      }

      if (event.key === "Escape") {
        event.preventDefault();
        close();
        return true;
      }

      if (event.key === "Enter") {
        const host = options[activeOptionIndex];
        if (!host) return false;
        event.preventDefault();
        chooseHost(host.serverId);
        return true;
      }

      if (options.length === 0) return false;
      event.preventDefault();
      const next = event.key === "ArrowDown" ? activeOptionIndex + 1 : activeOptionIndex - 1;
      if (next < 0) {
        setActiveIndex(options.length - 1);
        return true;
      }
      if (next >= options.length) {
        setActiveIndex(0);
        return true;
      }
      setActiveIndex(next);
      return true;
    },
    [activeOptionIndex, chooseHost, close, options],
  );
  const setWebOverlayScope = useWebOverlayRegistration({
    active: isWeb && request != null,
    layer: modalLayer,
    onKeyDown: handleWebOverlayKeyDown,
  });

  if (!request) return null;

  const modal = (
    <Modal visible transparent animationType="fade" onRequestClose={close} testID="host-chooser">
      <View style={styles.overlay}>
        <Pressable style={styles.backdrop} onPress={close} />
        <View ref={setWebOverlayScope} style={styles.panel}>
          <View style={styles.header}>
            <Text style={styles.title}>{request.title}</Text>
            <TextInput
              ref={inputRef}
              initialValue={query}
              onChangeText={handleQueryChange}
              placeholder="Search hosts..."
              placeholderTextColor={theme.colors.foregroundMuted}
              style={styles.input}
              autoCapitalize="none"
              autoCorrect={false}
              testID="host-chooser-search"
            />
          </View>
          <ScrollView
            style={styles.results}
            contentContainerStyle={styles.resultsContent}
            keyboardShouldPersistTaps="always"
            showsVerticalScrollIndicator={false}
          >
            {options.length === 0 ? <Text style={styles.emptyText}>No matching hosts</Text> : null}
            {options.map((host, index) => (
              <HostChooserRow
                key={host.serverId}
                host={host}
                active={index === activeOptionIndex}
                onChooseHost={chooseHost}
              />
            ))}
          </ScrollView>
        </View>
      </View>
    </Modal>
  );

  return createElement(OverlayLayerProvider, { layer: isWeb ? modalLayer : 0 }, modal);
}

const styles = StyleSheet.create((theme) => ({
  overlay: {
    flex: 1,
    justifyContent: "flex-start",
    alignItems: "center",
    paddingTop: theme.spacing[12],
  },
  backdrop: {
    ...StyleSheet.absoluteFill,
    backgroundColor: "rgba(0, 0, 0, 0.5)",
  },
  panel: {
    width: 640,
    maxWidth: "92%",
    maxHeight: "80%",
    borderWidth: 1,
    borderColor: theme.colors.border,
    borderRadius: theme.borderRadius.lg,
    backgroundColor: theme.colors.surface0,
    overflow: "hidden",
    ...theme.shadow.lg,
  },
  header: {
    gap: theme.spacing[3],
    paddingHorizontal: theme.spacing[4],
    paddingVertical: theme.spacing[4],
    borderBottomWidth: 1,
    borderBottomColor: theme.colors.border,
  },
  title: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.medium,
  },
  input: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    padding: 0,
    outlineStyle: "none",
  } as object,
  results: {
    maxHeight: 420,
  },
  resultsContent: {
    paddingVertical: theme.spacing[2],
  },
  row: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[3],
    minHeight: 56,
    paddingHorizontal: theme.spacing[4],
    paddingVertical: theme.spacing[3],
  },
  rowActive: {
    backgroundColor: theme.colors.surface1,
  },
  rowIconSlot: {
    width: 18,
    alignItems: "center",
    justifyContent: "center",
  },
  rowText: {
    flex: 1,
    minWidth: 0,
    gap: theme.spacing[1],
  },
  rowTitle: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
  },
  rowSubtitle: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  emptyText: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    paddingHorizontal: theme.spacing[4],
    paddingVertical: theme.spacing[3],
  },
}));
