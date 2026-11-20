import React, { createContext, useCallback, useContext } from "react";
import { Image, Text, View } from "react-native";
import { PressHighlight } from "@/components/ui/press-highlight";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { Download, FileText } from "lucide-react-native";
import { apiUrl } from "@/yyc/api";
import { openServerFile, serverImageSource } from "@/yyc/open-file";
import { useToast } from "@/contexts/toast-api-context";
import type { Theme } from "@/styles/theme";
import type { SentFile } from "@/yyc/sent-files-text";

export { isSentFilesCall, readSentCaption, readSentFiles, type SentFile } from "@/yyc/sent-files-text";

const mutedColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });
const ThemedFile = withUnistyles(FileText);
const ThemedDownload = withUnistyles(Download);

function formatSize(bytes: number | null): string {
  if (bytes === null) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/**
 * Where a conversation's files are read from: the URL of a path relative to its /workspace; with `sent` (a
 * SendUserFile call's id), the file as that send kept it.
 */
export type ConversationFileUrl = (relativePath: string, inline: boolean, sent?: string) => string;

/** The query string of a file URL: path, inline, sent. */
export function fileQuery(relativePath: string, inline: boolean, sent?: string): string {
  return `path=${encodeURIComponent(relativePath)}${inline ? "&inline=1" : ""}${sent ? `&sent=${encodeURIComponent(sent)}` : ""}`;
}

function sessionFileUrl(sessionId: string): ConversationFileUrl {
  return (relativePath, inline, sent) =>
    apiUrl(`/api/jarvis/sessions/${encodeURIComponent(sessionId)}/files/raw?${fileQuery(relativePath, inline, sent)}`);
}

/**
 * Overrides where the chat view's files come from (the conversation's own files when unset): a shared page renders the
 * same chat view and reads its files through the share.
 */
export const ConversationFilesContext = createContext<ConversationFileUrl | null>(null);

/** The file URLs of the conversation the chat view shows. */
export function useConversationFileUrl(sessionId: string): ConversationFileUrl {
  const override = useContext(ConversationFilesContext);
  return override ?? sessionFileUrl(sessionId);
}

/** Opens a file of the conversation's workspace (a path the reply names): images and PDFs in a tab, the rest
 * downloaded. */
export function openConversationFile(fileUrl: ConversationFileUrl, relativePath: string): void {
  const path = relativePath.replace(/^\/workspace\//, "").replace(/^\.\//, "");
  void openServerFile(fileUrl(path, true), path.split("/").pop() ?? path).catch(() => undefined);
}

/**
 * The files a reply hands over, as download cards under it (docs/paseo-ui.md): name and size, and a preview for
 * images. Only files Claude sent are offered; the server checks each download again.
 */
export function SentFilesCards({
  sessionId,
  files,
  caption,
  urlFor,
  isLastInSequence,
}: {
  sessionId: string;
  files: SentFile[];
  caption?: string | null;
  /** In a chat stream: spaced like a tool row (4 below, 16 after the last of a run). Unset on a shared page. */
  isLastInSequence?: boolean;
  /** Where a file is read from; the conversation's own files by default (a shared page reads through the share). */
  urlFor?: (file: SentFile, inline: boolean) => string;
}) {
  const fileUrl = useConversationFileUrl(sessionId);
  if (files.length === 0) return null;
  const resolveUrl = urlFor ?? ((file: SentFile, inline: boolean) => fileUrl(file.path, inline, file.sent));
  return (
    <View
      style={[
        styles.container,
        isLastInSequence === undefined ? null : isLastInSequence ? styles.lastInSequence : styles.inSequence,
      ]}
      testID="sent-files"
    >
      {caption ? <Text style={styles.caption}>{caption}</Text> : null}
      {files.map((file) => (
        <SentFileCard key={file.path} file={file} urlFor={resolveUrl} />
      ))}
    </View>
  );
}

function SentFileCard({
  file,
  urlFor,
}: {
  file: SentFile;
  urlFor: (file: SentFile, inline: boolean) => string;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const open = useCallback(() => {
    void openServerFile(urlFor(file, false), file.name).catch((error) =>
      toast.error(error instanceof Error ? error.message : String(error)),
    );
  }, [file, toast, urlFor]);
  return (
    <PressHighlight
      onPress={open}
      accessibilityRole="link"
      accessibilityLabel={t("conversation.files.download", { name: file.name })}
      style={({ hovered }) => [styles.card, hovered && styles.cardHovered]}
      highlightStyle={styles.cardPressed}
      testID="sent-file-card"
    >
      {file.isImage ? (
        <Image
          source={serverImageSource(urlFor(file, true))}
          style={styles.preview}
          resizeMode="cover"
          accessibilityIgnoresInvertColors
        />
      ) : null}
      <View style={styles.row}>
        <View style={styles.iconSlot}>
          <ThemedFile size={16} uniProps={mutedColorMapping} />
        </View>
        <View style={styles.text}>
          <Text style={styles.name} numberOfLines={1}>
            {file.name}
          </Text>
          {file.size !== null ? <Text style={styles.size}>{formatSize(file.size)}</Text> : null}
        </View>
        <ThemedDownload size={16} uniProps={mutedColorMapping} />
      </View>
    </PressHighlight>
  );
}

const styles = StyleSheet.create((theme) => ({
  container: {
    gap: theme.spacing[2],
    marginVertical: theme.spacing[2],
    maxWidth: 420,
  },
  // A tool row's spacing (message.tsx expandableBadgeStylesheet).
  inSequence: {
    marginTop: 0,
    marginBottom: theme.spacing[1],
  },
  lastInSequence: {
    marginTop: 0,
    marginBottom: theme.spacing[4],
  },
  // The reply's own text (markdown/styles.ts paragraphs).
  caption: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.content,
    lineHeight: Math.round(theme.fontSize.content * 1.4),
  },
  // The user's attachment pill's frame (components/attachment-pill.tsx).
  card: {
    borderWidth: 1,
    borderColor: theme.colors.borderAccent,
    borderRadius: theme.borderRadius.md,
    backgroundColor: theme.colors.surface1,
    overflow: "hidden",
  },
  cardHovered: {
    backgroundColor: theme.colors.surface2,
  },
  // a finger on the card (the phone apps, PressHighlight): the shared overlay, in the card's corners (its style is a
  // function, whose corners the highlight cannot read)
  cardPressed: {
    backgroundColor: theme.colors.interactionHighlight,
    borderRadius: theme.borderRadius.md,
  },
  preview: {
    width: "100%",
    height: 200,
    backgroundColor: theme.colors.surface2,
  },
  row: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    minHeight: 48,
    paddingHorizontal: theme.spacing[3],
    paddingVertical: theme.spacing[2],
  },
  iconSlot: {
    width: 18,
    alignItems: "center",
  },
  text: {
    flex: 1,
    minWidth: 0,
  },
  name: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
  },
  size: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
}));
