import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  type ComponentProps,
  type MutableRefObject,
  type PropsWithChildren,
  type ReactNode,
} from "react";
import * as Haptics from "expo-haptics";
import { useTranslation } from "react-i18next";
import { type GestureResponderEvent, type PressableStateCallbackType } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import {
  Archive,
  Circle,
  CircleCheck,
  Copy,
  MoreVertical,
  Pin,
  PinOff,
  Tag,
} from "lucide-react-native";
import { isNative, isWeb } from "@/constants/platform";
import { getForgePresentation, normalizeForge } from "@/git/forge";
import type { SidebarWorkspaceEntry } from "@/hooks/use-sidebar-workspaces-list";
import { useAppSettings } from "@/hooks/use-settings";
import { densityIconProps } from "@/styles/density";
import type { Theme } from "@/styles/theme";
import type { ShortcutKey } from "@/utils/format-shortcut";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
  useContextMenu,
} from "@/components/ui/context-menu";
import {
  CONVERSATION_MENU_WIDTH,
  DELETE_ICON,
  PIN_ICON,
  RENAME_ICON,
  UNPIN_ICON,
  SHARE_ICON,
} from "@/screens/workspace/conversation-menu-items";
import { openConversationDialog } from "@/yyc/conversation-dialog-store";
import { useSharingAvailable } from "@/yyc/me";
import { OpenInFileManagerMenuItem } from "@/workspace/open-in-file-manager/menu-item";
import { resolveSidebarWorkspaceAccessibilityLabel } from "@/components/sidebar/sidebar-workspace-title";
import {
  workspaceServiceLabelKey,
  type WorkspaceServiceSummary,
} from "@/components/sidebar/workspace-meta-row";
import {
  useWorkspaceLabelMenuPages,
  WORKSPACE_LABEL_PAGE_ID,
  type WorkspaceLabelTarget,
} from "@/workspace-labels/picker";

const foregroundColorMapping = (theme: Theme) => ({ color: theme.colors.foreground });
const foregroundMutedColorMapping = (theme: Theme) => ({
  color: theme.colors.foregroundMuted,
});

const ThemedMoreVertical = withUnistyles(MoreVertical);
const ThemedCopy = withUnistyles(Copy);
const ThemedArchive = withUnistyles(Archive);
const ThemedCircle = withUnistyles(Circle);
const ThemedCircleCheck = withUnistyles(CircleCheck);
const ThemedPin = withUnistyles(Pin);
const ThemedPinOff = withUnistyles(PinOff);
const ThemedTag = withUnistyles(Tag);

const copyLeadingIcon = <ThemedCopy size={14} uniProps={foregroundMutedColorMapping} />;
const markAsReadLeadingIcon = (
  <ThemedCircleCheck size={14} uniProps={foregroundMutedColorMapping} />
);
const markAsUnreadLeadingIcon = <ThemedCircle size={14} uniProps={foregroundMutedColorMapping} />;
const pinLeadingIcon = <ThemedPin size={14} uniProps={foregroundMutedColorMapping} />;
const unpinLeadingIcon = <ThemedPinOff size={14} uniProps={foregroundMutedColorMapping} />;

const triggerIconProps = densityIconProps("sm", foregroundMutedColorMapping);
const triggerIconHoveredProps = densityIconProps("sm", foregroundColorMapping);

function renderTriggerIcon({ hovered }: { hovered?: boolean }) {
  return <ThemedMoreVertical uniProps={hovered ? triggerIconHoveredProps : triggerIconProps} />;
}

export interface SidebarWorkspaceMenuProps {
  workspaceKey: string;
  serverId?: string;
  workspaceId?: string;
  workspaceLabels?: readonly string[];
  onCopyPath?: () => void;
  onCopyBranchName?: () => void;
  onRename?: () => void;
  onMarkAsRead?: () => void;
  onMarkAsUnread?: () => void;
  /** Delete, when the deployment offers it (`conversationDelete`); the menu's other items stay without it. */
  onArchive?: () => void;
  archiveLabel?: string;
  archiveStatus?: "idle" | "pending" | "success";
  archivePendingLabel?: string;
  archiveShortcutKeys?: ShortcutKey[][] | null;
  isPinned?: boolean;
  onTogglePin?: () => void;
  openInFileManagerPath?: string | null;
  /**
   * Lifted so the row that reveals the kebab can keep it mounted while its menu is up. See
   * `useOpenKebabMenuVisibility`.
   */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}

type SidebarWorkspaceMenuItemsProps = Omit<SidebarWorkspaceMenuProps, "open" | "onOpenChange">;

type MenuSurface = "context" | "dropdown";

function WorkspaceMenuItem({
  surface,
  children,
  ...props
}: PropsWithChildren<
  Omit<ComponentProps<typeof DropdownMenuItem>, "children"> & { surface: MenuSurface }
>) {
  if (surface === "context") {
    return <ContextMenuItem {...props}>{children}</ContextMenuItem>;
  }
  return <DropdownMenuItem {...props}>{children}</DropdownMenuItem>;
}

function SidebarWorkspaceMenuItems({
  surface,
  workspaceKey,
  serverId,
  workspaceId,
  onCopyPath,
  onCopyBranchName,
  onRename,
  onMarkAsRead,
  onMarkAsUnread,
  onArchive,
  archiveLabel,
  archiveStatus,
  archivePendingLabel,
  isPinned,
  onTogglePin,
  openInFileManagerPath,
}: SidebarWorkspaceMenuItemsProps & { surface: MenuSurface }): ReactNode {
  const { t } = useTranslation();
  const sharing = useSharingAvailable();
  const labelLeading = useMemo(
    () => <ThemedTag size={14} uniProps={foregroundMutedColorMapping} />,
    [],
  );

  // A conversation's menu: rename, share, delete (docs/paseo-ui.md). No paths, branches, read state, pins,
  // labels or folders to act on.
  return (
    <>
      {onTogglePin ? (
        <WorkspaceMenuItem
          surface={surface}
          testID={`sidebar-workspace-menu-pin-${workspaceKey}`}
          leading={isPinned ? UNPIN_ICON : PIN_ICON}
          onSelect={onTogglePin}
        >
          {isPinned ? t("conversation.menu.unpin") : t("conversation.menu.pin")}
        </WorkspaceMenuItem>
      ) : null}
      {onRename ? (
        <WorkspaceMenuItem
          surface={surface}
          testID={`sidebar-workspace-menu-rename-${workspaceKey}`}
          leading={RENAME_ICON}
          onSelect={onRename}
        >
          {t("sidebar.workspace.actions.rename")}
        </WorkspaceMenuItem>
      ) : null}
      {sharing && serverId && workspaceId ? (
        <WorkspaceMenuItem
          surface={surface}
          testID={`sidebar-workspace-menu-share-${workspaceKey}`}
          leading={SHARE_ICON}
          onSelect={() => openConversationDialog({ kind: "share", serverId, workspaceId })}
        >
          {t("conversation.menu.share")}
        </WorkspaceMenuItem>
      ) : null}
      {onArchive && (onRename || sharing) ? (
        surface === "context" ? (
          <ContextMenuSeparator />
        ) : (
          <DropdownMenuSeparator />
        )
      ) : null}
      {onArchive ? (
        <WorkspaceMenuItem
          surface={surface}
          testID={`sidebar-workspace-menu-archive-${workspaceKey}`}
          leading={DELETE_ICON}
          destructive
          status={archiveStatus}
          pendingLabel={archivePendingLabel}
          onSelect={onArchive}
        >
          {archiveLabel ?? t("sidebar.workspace.actions.archive")}
        </WorkspaceMenuItem>
      ) : null}
    </>
  );
}

export function SidebarWorkspaceMenu({
  workspaceKey,
  serverId,
  workspaceId,
  workspaceLabels,
  onCopyPath,
  onCopyBranchName,
  onRename,
  onMarkAsRead,
  onMarkAsUnread,
  onArchive,
  archiveLabel,
  archiveStatus,
  archivePendingLabel,
  archiveShortcutKeys,
  isPinned,
  onTogglePin,
  openInFileManagerPath,
  open,
  onOpenChange,
}: SidebarWorkspaceMenuProps) {
  const { t } = useTranslation();
  const workspaceTarget = useMemo<WorkspaceLabelTarget | null>(
    () =>
      serverId && workspaceId ? { serverId, workspaceId, labels: workspaceLabels ?? [] } : null,
    [serverId, workspaceId, workspaceLabels],
  );
  const pages = useWorkspaceLabelMenuPages(workspaceTarget);
  return (
    <DropdownMenu compactMode="sheet" open={open} onOpenChange={onOpenChange}>
      <DropdownMenuTrigger
        hitSlop={8}
        style={triggerStyle}
        accessibilityRole={isWeb ? undefined : "button"}
        accessibilityLabel={t("sidebar.workspace.actions.menu")}
        testID={`sidebar-workspace-kebab-${workspaceKey}`}
      >
        {renderTriggerIcon}
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="end"
        width={CONVERSATION_MENU_WIDTH}
        pages={pages}
        sheetTitle={t("sidebar.workspace.actions.menu")}
      >
        <SidebarWorkspaceMenuItems
          surface="dropdown"
          workspaceKey={workspaceKey}
          serverId={serverId}
          workspaceId={workspaceId}
          workspaceLabels={workspaceLabels}
          onCopyPath={onCopyPath}
          onCopyBranchName={onCopyBranchName}
          onRename={onRename}
          onMarkAsRead={onMarkAsRead}
          onMarkAsUnread={onMarkAsUnread}
          onArchive={onArchive}
          archiveLabel={archiveLabel}
          archiveStatus={archiveStatus}
          archivePendingLabel={archivePendingLabel}
          archiveShortcutKeys={archiveShortcutKeys}
          isPinned={isPinned}
          onTogglePin={onTogglePin}
          openInFileManagerPath={openInFileManagerPath}
        />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

type ContextTriggerProps = Omit<
  ComponentProps<typeof ContextMenuTrigger>,
  "children" | "enabledOnMobile" | "highlightStyle"
>;

/** What opens a row's menu from outside it: the long press of a row that can be dragged. */
export type RowMenuOpener = Pick<ReturnType<typeof useContextMenu>, "setAnchorRect" | "setOpen">;

function RowMenuOpenerBridge({ openerRef }: { openerRef: MutableRefObject<RowMenuOpener | null> }): null {
  const menu = useContextMenu();
  useEffect(() => {
    openerRef.current = menu;
    return () => {
      openerRef.current = null;
    };
  }, [menu, openerRef]);
  return null;
}

/**
 * A row's long-press menu (Deyao, 2026-11-20: hold opens the menu at the row, hold and move drags): a row that can be
 * dragged hands `menuController` to its drag's long press, which opens the menu when the finger has not moved;
 * `openerRef` goes to the row's SidebarWorkspaceContextMenu.
 */
export function useRowLongPressMenu(draggable: boolean): {
  openerRef: MutableRefObject<RowMenuOpener | null>;
  menuController: RowMenuOpener | null;
} {
  const openerRef = useRef<RowMenuOpener | null>(null);
  const menuController = useMemo<RowMenuOpener>(
    () => ({
      setAnchorRect: (rect) => openerRef.current?.setAnchorRect(rect),
      setOpen: (open) => openerRef.current?.setOpen(open),
    }),
    [],
  );
  return { openerRef, menuController: draggable ? menuController : null };
}

// a long press opens the menu after as long as on a row that can be dragged (use-long-press-drag-interaction)
const LONG_PRESS_MENU_DELAY_MS = 450;

export function SidebarWorkspaceContextMenu({
  children,
  contextMenuOpen,
  onContextMenuOpenChange,
  workspace,
  leadingProjectName,
  hostBadgeLabel,
  serviceSummary,
  workspaceKey,
  onCopyPath,
  onCopyBranchName,
  onRename,
  onMarkAsRead,
  onMarkAsUnread,
  onArchive,
  archiveLabel,
  archiveStatus,
  archivePendingLabel,
  archiveShortcutKeys,
  isPinned,
  onTogglePin,
  openInFileManagerPath,
  accessibilityLabel,
  highlightStyle,
  longPressOpens = false,
  openerRef,
  onLongPress,
  ...triggerProps
}: PropsWithChildren<
  SidebarWorkspaceMenuItemsProps &
    ContextTriggerProps & {
      contextMenuOpen: boolean;
      onContextMenuOpenChange: (open: boolean) => void;
      workspace: SidebarWorkspaceEntry;
      leadingProjectName?: string | null;
      hostBadgeLabel?: string | null;
      serviceSummary?: WorkspaceServiceSummary | null;
      highlightStyle: ComponentProps<typeof ContextMenuTrigger>["highlightStyle"];
      /**
       * On the phone apps a long press opens the menu at the row, like ⋯ (Deyao, 2026-11-20). A row that can be
       * dragged opens it from its own long press instead (`openerRef`), so that holding and then moving drags it.
       */
      longPressOpens?: boolean;
      openerRef?: MutableRefObject<RowMenuOpener | null>;
    }
>) {
  const {
    settings: { workspaceTitleSource },
  } = useAppSettings();
  const { t } = useTranslation();
  const pullRequestLabel = workspace.prHint
    ? t("workspace.git.pr.accessibility.pullRequest", {
        number: workspace.prHint.number,
        context: getForgePresentation(normalizeForge(workspace.prHint.forge)).changeRequestContext,
      })
    : null;
  const rowAccessibilityLabel = resolveSidebarWorkspaceAccessibilityLabel({
    workspace,
    workspaceTitleSource,
    leadingProjectName,
    hostBadgeLabel,
    pullRequestLabel,
    serviceLabel: serviceSummary
      ? t(workspaceServiceLabelKey(serviceSummary), { name: serviceSummary.name })
      : null,
    t,
  });
  const workspaceTarget = useMemo<WorkspaceLabelTarget>(
    () => ({
      serverId: workspace.serverId,
      workspaceId: workspace.workspaceId,
      labels: workspace.labels ?? [],
    }),
    [workspace],
  );
  const pages = useWorkspaceLabelMenuPages(workspaceTarget);
  const handleLongPress = useCallback(
    (event: GestureResponderEvent) => {
      if (longPressOpens && isNative) void Haptics.selectionAsync().catch(() => {});
      onLongPress?.(event);
    },
    [longPressOpens, onLongPress],
  );

  return (
    <ContextMenu
      open={contextMenuOpen}
      onOpenChange={onContextMenuOpenChange}
      compactMode={isNative ? "popover" : "sheet"}
    >
      {openerRef ? <RowMenuOpenerBridge openerRef={openerRef} /> : null}
      <ContextMenuTrigger
        {...triggerProps}
        enabledOnMobile={longPressOpens}
        longPressDelayMs={LONG_PRESS_MENU_DELAY_MS}
        onLongPress={handleLongPress}
        accessibilityLabel={accessibilityLabel ?? rowAccessibilityLabel}
        highlightStyle={highlightStyle}
      >
        {children}
      </ContextMenuTrigger>
      <ContextMenuContent
        align="start"
        width={CONVERSATION_MENU_WIDTH}
        testID={`sidebar-workspace-context-menu-${workspaceKey}`}
        pages={pages}
      >
        <SidebarWorkspaceMenuItems
          surface="context"
          workspaceKey={workspaceKey}
          serverId={workspaceTarget.serverId}
          workspaceId={workspaceTarget.workspaceId}
          workspaceLabels={workspaceTarget.labels}
          onCopyPath={onCopyPath}
          onCopyBranchName={onCopyBranchName}
          onRename={onRename}
          onMarkAsRead={onMarkAsRead}
          onMarkAsUnread={onMarkAsUnread}
          onArchive={onArchive}
          archiveLabel={archiveLabel}
          archiveStatus={archiveStatus}
          archivePendingLabel={archivePendingLabel}
          archiveShortcutKeys={archiveShortcutKeys}
          isPinned={isPinned}
          onTogglePin={onTogglePin}
          openInFileManagerPath={openInFileManagerPath}
        />
      </ContextMenuContent>
    </ContextMenu>
  );
}

function triggerStyle({ hovered = false }: PressableStateCallbackType & { hovered?: boolean }) {
  return [styles.trigger, hovered && styles.triggerHovered];
}

const styles = StyleSheet.create((theme) => ({
  trigger: {
    // A finger's 44 px on a phone (Deyao, 2026-11-18), the desktop's 18 px glyph box from md up.
    width: { xs: 44, md: 18 },
    height: { xs: 44, md: 18 },
    alignItems: "center",
    justifyContent: "center",
    borderRadius: 4,
    marginLeft: 2,
    // Center the compact box on the 20px title line it is anchored to.
    marginTop: { xs: -12, md: 0 },
    // MoreVertical paints only around the center of its SVG. Keep the padded hit box, but
    // pull the painted dots through that unused view-box space onto the trailing-content rail.
    marginRight: { xs: -20, md: -7 },
  },
  triggerHovered: {
    backgroundColor: theme.colors.surface2,
  },
}));
