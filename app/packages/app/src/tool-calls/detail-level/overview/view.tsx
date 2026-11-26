import React, { memo, useCallback, useRef, type ReactNode } from "react";
import { ScrollView, View } from "react-native";
import { Wrench } from "lucide-react-native";
import { StyleSheet } from "react-native-unistyles";
import { useRowAnchor } from "@/agent-stream/row-anchor";
import { IndentRail } from "@/components/indent-rail";
import { ExpandableBadge } from "@/components/message";
import { useOverviewSummaryLabel } from "./label";
import type { OverviewToolCallGroup } from "./model";

interface OverviewGroupProps {
  group: OverviewToolCallGroup;
  expanded: boolean;
  isLastInSequence: boolean;
  onExpandedChange: (groupId: string, expanded: boolean) => void;
  children: ReactNode;
}

const TOOL_CALL_GROUP_MAX_HEIGHT = 400;

export const OverviewToolCallGroupView = memo(function OverviewToolCallGroupView({
  group,
  expanded,
  isLastInSequence,
  onExpandedChange,
  children,
}: OverviewGroupProps) {
  const scrollRef = useRef<ScrollView>(null);
  const aggregateSummary = useOverviewSummaryLabel(group.summary);
  const scrollToLatest = useCallback(() => {
    scrollRef.current?.scrollToEnd({ animated: false });
  }, []);
  const toggle = useCallback(() => {
    onExpandedChange(group.run.id, !expanded);
  }, [expanded, group.run.id, onExpandedChange]);
  // Where the list holds a row's header still as it grows (the phone's, agent-stream/row-anchor.tsx), the list does
  // the scrolling: a box of the group's own, capped, would grow less than its calls and the header would slide by the
  // difference. Elsewhere the calls scroll in a box of at most 400 px, newest in view.
  const rowAnchor = useRowAnchor();
  const renderDetails = useCallback(
    () =>
      rowAnchor ? (
        <View style={styles.content}>
          <IndentRail>{children}</IndentRail>
        </View>
      ) : (
        <ScrollView
          ref={scrollRef}
          style={styles.scroll}
          contentContainerStyle={styles.content}
          nestedScrollEnabled
          showsVerticalScrollIndicator
          onContentSizeChange={scrollToLatest}
        >
          <IndentRail>{children}</IndentRail>
        </ScrollView>
      ),
    [children, rowAnchor, scrollToLatest],
  );

  // Opens in place on every screen, as a single tool call does (Deyao, 2026-11-20: no bottom sheet on a phone)
  return (
    <ExpandableBadge
      testID="tool-call-group"
      label={aggregateSummary}
      icon={Wrench}
      isLoading={group.isLoading}
      isExpanded={expanded}
      isLastInSequence={isLastInSequence}
      onToggle={toggle}
      renderDetails={renderDetails}
      borderlessWhenExpanded
    />
  );
});

const styles = StyleSheet.create((theme) => ({
  scroll: {
    maxHeight: TOOL_CALL_GROUP_MAX_HEIGHT,
  },
  content: {
    paddingTop: theme.spacing[1],
    paddingHorizontal: 13,
  },
}));
