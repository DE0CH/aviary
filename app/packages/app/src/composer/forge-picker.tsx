import { useCallback, useMemo, type ReactElement, type RefObject } from "react";
import { View } from "react-native";
import { withUnistyles } from "react-native-unistyles";
import { CircleDot, GitPullRequest } from "lucide-react-native";
import type { ForgeSearchItem } from "@getpaseo/protocol/messages";
import { Combobox, ComboboxItem, type ComboboxOption } from "@/components/ui/combobox";
import { ICON_SIZE, type Theme } from "@/styles/theme";

// The composer's pull request / issue picker (the attach menu's forge search), its own chunk: fetched the first time
// the picker opens (composer/index.tsx, docs/paseo-ui.md "Loading"), so the message box's first load has no Combobox.

const ThemedGitPullRequest = withUnistyles(GitPullRequest);
const ThemedCircleDot = withUnistyles(CircleDot);
const iconForegroundMutedMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });

interface GithubPickerOptionProps {
  label: string;
  testID: string;
  active: boolean;
  selected: boolean;
  item: ForgeSearchItem;
  onToggle: (item: ForgeSearchItem) => void;
}

function GithubPickerOption({
  label,
  testID,
  active,
  selected,
  item,
  onToggle,
}: GithubPickerOptionProps) {
  const handlePress = useCallback(() => {
    onToggle(item);
  }, [onToggle, item]);
  const leadingSlot = useMemo(
    () =>
      item.kind === "change_request" ? (
        <ThemedGitPullRequest size={ICON_SIZE.sm} uniProps={iconForegroundMutedMapping} />
      ) : (
        <ThemedCircleDot size={ICON_SIZE.sm} uniProps={iconForegroundMutedMapping} />
      ),
    [item.kind],
  );
  return (
    <ComboboxItem
      testID={testID}
      label={label}
      selected={selected}
      active={active}
      onPress={handlePress}
      leadingSlot={leadingSlot}
    />
  );
}

export interface ForgeSearchPickerProps {
  visible: boolean;
  options: ComboboxOption[];
  searchPlaceholder: string;
  title: string;
  onOpenChange: (open: boolean) => void;
  onSearchQueryChange: (query: string) => void;
  anchorRef: RefObject<View | null>;
  emptyText: string;
  /** The search result behind an option and whether it is attached, or null when it is gone. */
  resolveOption: (optionId: string) => { item: ForgeSearchItem; selected: boolean } | null;
  onToggle: (item: ForgeSearchItem) => void;
}

function noop() {}

export function ForgeSearchPicker({
  visible,
  options,
  searchPlaceholder,
  title,
  onOpenChange,
  onSearchQueryChange,
  anchorRef,
  emptyText,
  resolveOption,
  onToggle,
}: ForgeSearchPickerProps): ReactElement {
  const renderOption = useCallback(
    ({ option, active }: { option: ComboboxOption; selected: boolean; active: boolean }) => {
      const resolved = resolveOption(option.id);
      if (!resolved) {
        return <View key={option.id} />;
      }
      return (
        <GithubPickerOption
          key={option.id}
          testID={`composer-github-option-${option.id}`}
          label={option.label}
          selected={resolved.selected}
          active={active}
          item={resolved.item}
          onToggle={onToggle}
        />
      );
    },
    [resolveOption, onToggle],
  );
  return (
    <Combobox
      options={options}
      value=""
      onSelect={noop}
      keepOpenOnSelect
      searchable
      searchPlaceholder={searchPlaceholder}
      title={title}
      open={visible}
      onOpenChange={onOpenChange}
      onSearchQueryChange={onSearchQueryChange}
      desktopPlacement="top-start"
      anchorRef={anchorRef}
      emptyText={emptyText}
      renderOption={renderOption}
    />
  );
}
