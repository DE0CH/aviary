import React, { useCallback } from "react";
import { Image, Linking, Pressable, Text, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { Download, FileText } from "lucide-react-native";
import { apiUrl } from "@/yyc/api";
import type { Theme } from "@/styles/theme";

/** One file Claude sent with SendUserFile (the bridge puts these on the tool call's metadata). */
export interface SentFile {
  path: string;
  name: string;
  size: number | null;
  mediaType: string | null;
  isImage: boolean;
}

/** The caption Claude gave the files (SendUserFile's `caption` input), if any. */
export function readSendUserFileCaption(detail: unknown): string | null {
  const input = (detail as { input?: unknown } | undefined)?.input;
  const caption = (input as { caption?: unknown } | null)?.caption;
  return typeof caption === "string" && caption.trim() ? caption.trim() : null;
}

export function readSentFiles(metadata: unknown): SentFile[] {
  const files = (metadata as { sentFiles?: unknown } | undefined)?.sentFiles;
  return Array.isArray(files) ? (files as SentFile[]) : [];
}

const mutedColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });
const ThemedFile = withUnistyles(FileText);
const ThemedDownload = withUnistyles(Download);

function formatSize(bytes: number | null): string {
  if (bytes === null) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function fileUrl(sessionId: string, file: SentFile, inline: boolean): string {
  const query = `path=${encodeURIComponent(file.path)}${inline ? "&inline=1" : ""}`;
  return apiUrl(`/api/jarvis/sessions/${encodeURIComponent(sessionId)}/files/raw?${query}`);
}

/** Opens a file of the conversation's workspace (a path the reply names): images and PDFs in a tab, the rest
 * downloaded. */
export function openConversationFile(sessionId: string, relativePath: string): void {
  const path = relativePath.replace(/^\/workspace\//, "").replace(/^\.\//, "");
  const query = `path=${encodeURIComponent(path)}&inline=1`;
  void Linking.openURL(apiUrl(`/api/jarvis/sessions/${encodeURIComponent(sessionId)}/files/raw?${query}`));
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
}: {
  sessionId: string;
  files: SentFile[];
  caption?: string | null;
  /** Where a file is read from; the conversation's own files by default (a shared page reads through the share). */
  urlFor?: (file: SentFile, inline: boolean) => string;
}) {
  if (files.length === 0) return null;
  const resolveUrl = urlFor ?? ((file: SentFile, inline: boolean) => fileUrl(sessionId, file, inline));
  return (
    <View style={styles.container} testID="sent-files">
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
  const open = useCallback(() => {
    void Linking.openURL(urlFor(file, false));
  }, [file, urlFor]);
  return (
    <Pressable
      onPress={open}
      accessibilityRole="link"
      accessibilityLabel={t("conversation.files.download", { name: file.name })}
      style={({ hovered }) => [styles.card, hovered && styles.cardHovered]}
      testID="sent-file-card"
    >
      {file.isImage ? (
        <Image
          source={{ uri: urlFor(file, true) }}
          style={styles.preview}
          resizeMode="cover"
          accessibilityIgnoresInvertColors
        />
      ) : null}
      <View style={styles.row}>
        <ThemedFile size={16} uniProps={mutedColorMapping} />
        <View style={styles.text}>
          <Text style={styles.name} numberOfLines={1}>
            {file.name}
          </Text>
          {file.size !== null ? <Text style={styles.size}>{formatSize(file.size)}</Text> : null}
        </View>
        <ThemedDownload size={16} uniProps={mutedColorMapping} />
      </View>
    </Pressable>
  );
}

const styles = StyleSheet.create((theme) => ({
  container: {
    gap: theme.spacing[2],
    marginVertical: theme.spacing[2],
    maxWidth: 420,
  },
  caption: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
  },
  card: {
    borderWidth: 1,
    borderColor: theme.colors.border,
    borderRadius: theme.borderRadius.lg,
    backgroundColor: theme.colors.surface1,
    overflow: "hidden",
  },
  cardHovered: {
    backgroundColor: theme.colors.surface2,
  },
  preview: {
    width: "100%",
    height: 200,
    backgroundColor: theme.colors.surface2,
  },
  row: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[3],
    paddingHorizontal: theme.spacing[3],
    paddingVertical: theme.spacing[2],
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
