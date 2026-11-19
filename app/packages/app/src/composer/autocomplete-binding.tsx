import { useCallback, useImperativeHandle, useSyncExternalStore } from "react";
import type { View } from "react-native";
import { useStore } from "zustand";
import type { StoreApi } from "zustand/vanilla";
import { AutocompletePopover } from "@/components/ui/autocomplete-popover";
import type { AutocompleteOption } from "@/components/ui/autocomplete";
import { useAgentAutocomplete } from "@/hooks/use-agent-autocomplete";
import type { ComposerInputModePresentation } from "@/composer/input-mode";
import { useComposerKeyboardScope } from "@/composer/keyboard-scope";
import type { ComposerRecall } from "@/composer/recall";
import type { ComposerKeyPressEvent, MessageInputRef } from "./input/input";
import type { ComposerTextSource } from "./text-source";

export interface ComposerAutocompleteHandle {
  onKeyPress: (event: ComposerKeyPressEvent) => boolean;
}

function ComposerAutocomplete(props: React.ComponentProps<typeof AutocompletePopover>) {
  const { isActiveComposer } = useComposerKeyboardScope();
  return <AutocompletePopover {...props} visible={isActiveComposer && props.visible} />;
}

/** The `/` picker over the message box: what the composer's text and cursor complete to, for the mode. */
export function ComposerAutocompleteBinding({
  text,
  cursor,
  recall,
  configuration,
  inputRef,
  children,
  anchorStyle,
  mode,
  show,
  ref,
}: {
  text: ComposerTextSource;
  cursor: StoreApi<number>;
  recall: ComposerRecall;
  configuration: Omit<
    Parameters<typeof useAgentAutocomplete>[0],
    "userInput" | "cursorIndex" | "onAutocompleteApplied" | "fileMentions" | "clientCommands"
  >;
  inputRef: React.RefObject<MessageInputRef | null>;
  children: React.ReactNode;
  anchorStyle: React.ComponentProps<typeof View>["style"];
  mode: ComposerInputModePresentation;
  show: boolean;
  ref: React.Ref<ComposerAutocompleteHandle>;
}) {
  const userInput = useSyncExternalStore(text.subscribe, text.getSnapshot, text.getSnapshot);
  const cursorIndex = useStore(cursor);
  const autocomplete = useAgentAutocomplete({
    ...configuration,
    userInput,
    cursorIndex: Math.min(cursorIndex, userInput.length),
    onAutocompleteApplied: () => inputRef.current?.focus(),
    fileMentions: mode.showFileAutocomplete,
    clientCommands: mode.showClientCommands,
  });
  // A recalled message is sent as is; its leading `/command` must not open the picker.
  // Nothing to list (a new chat, a `/` in a path, a name no command has) shows no box.
  const isShown =
    show &&
    mode.showCommandAutocomplete &&
    !recall.isShowing(userInput) &&
    autocomplete.isVisible &&
    (autocomplete.options.length > 0 || Boolean(autocomplete.errorMessage));
  const handleKeyPress = autocomplete.onKeyPress;
  useImperativeHandle(ref, () => ({ onKeyPress: (event) => isShown && handleKeyPress(event) }), [
    isShown,
    handleKeyPress,
  ]);
  const selectOption = autocomplete.onSelectOption;
  const onSelect = useCallback(
    (option: AutocompleteOption) => selectOption(option, inputRef.current?.getInputSnapshot()),
    [selectOption, inputRef],
  );
  return (
    <ComposerAutocomplete
      visible={isShown}
      anchorStyle={anchorStyle}
      options={autocomplete.options}
      selectedIndex={autocomplete.selectedIndex}
      onSelect={onSelect}
      isLoading={autocomplete.isLoading}
      errorMessage={autocomplete.errorMessage}
      loadingText={autocomplete.loadingText}
      emptyText={autocomplete.emptyText}
    >
      {children}
    </ComposerAutocomplete>
  );
}
