import type { ReactElement } from "react";
import { DropdownMenuContent, DropdownMenuItem } from "@/components/ui/dropdown-menu";
import type { AttachmentMenuItem } from "./input";

// The attach menu's surface and items: the menu engine's popover and sheet are their own chunk, fetched at idle or
// on the first hover, press or open of the attach button, whichever comes first (docs/paseo-ui.md "Loading"); the
// trigger and the menu's state stay in the composer.
export function AttachmentMenuContent({
  items,
  sheetTitle,
}: {
  items: AttachmentMenuItem[];
  sheetTitle: string;
}): ReactElement {
  return (
    <DropdownMenuContent
      side="top"
      align="start"
      offset={8}
      minWidth={220}
      testID="message-input-attachment-menu"
      sheetTitle={sheetTitle}
    >
      {items.map((item) => (
        <DropdownMenuItem
          key={item.id}
          testID={`message-input-attachment-menu-item-${item.id}`}
          disabled={item.disabled}
          onSelect={item.onSelect}
          leading={item.icon ?? null}
        >
          {item.label}
        </DropdownMenuItem>
      ))}
    </DropdownMenuContent>
  );
}
