/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import React from 'react';
import { Box, Text } from 'ink';
import { Colors } from '../../colors.js';
import { TaskResultDisplay } from '@qwen-code/qwen-code-core';

export interface SubagentExecutionDisplayProps {
  data: TaskResultDisplay;
}

/**
 * Component to display subagent execution progress and results.
 * This is now a pure component that renders the provided SubagentExecutionResultDisplay data.
 * Real-time updates are handled by the parent component updating the data prop.
 */
export const SubagentExecutionDisplay: React.FC<
  SubagentExecutionDisplayProps
> = ({ data }) => (
  <Box flexDirection="column" paddingX={1}>
    {/* Header with subagent name and status */}
    <Box flexDirection="row" marginBottom={1}>
      <StatusDot status={data.status} />
      <Text bold color={Colors.AccentBlue}>
        {data.subagentName}
      </Text>
      <Text color={Colors.Gray}> • </Text>
      <StatusIndicator status={data.status} />
    </Box>

    {/* Task description */}
    <Box flexDirection="row" marginBottom={1}>
      <Text color={Colors.Gray}>Task: </Text>
      <Text wrap="wrap">{data.taskDescription}</Text>
    </Box>

    {/* Progress section for running tasks */}
    {data.status === 'running' && (
      <ProgressSection progress={data.progress || { toolCalls: [] }} />
    )}

    {/* Results section for completed/failed tasks */}
    {(data.status === 'completed' || data.status === 'failed') && (
      <ResultsSection data={data} />
    )}
  </Box>
);

/**
 * Status dot component with similar height as text
 */
const StatusDot: React.FC<{
  status: TaskResultDisplay['status'];
}> = ({ status }) => {
  const color = React.useMemo(() => {
    switch (status) {
      case 'running':
        return Colors.AccentYellow;
      case 'completed':
        return Colors.AccentGreen;
      case 'failed':
        return Colors.AccentRed;
      default:
        return Colors.Gray;
    }
  }, [status]);

  return (
    <Box marginRight={1}>
      <Text color={color}>●</Text>
    </Box>
  );
};

/**
 * Status indicator component
 */
const StatusIndicator: React.FC<{
  status: TaskResultDisplay['status'];
}> = ({ status }) => {
  switch (status) {
    case 'running':
      return <Text color={Colors.AccentYellow}>Running</Text>;
    case 'completed':
      return <Text color={Colors.AccentGreen}>Completed</Text>;
    case 'failed':
      return <Text color={Colors.AccentRed}>Failed</Text>;
    default:
      return <Text color={Colors.Gray}>Unknown</Text>;
  }
};

/**
 * Progress section for running executions
 */
const ProgressSection: React.FC<{
  progress: {
    toolCalls?: Array<{
      name: string;
      status: 'executing' | 'success' | 'failed';
      error?: string;
      args?: Record<string, unknown>;
      result?: string;
      returnDisplay?: string;
    }>;
  };
}> = ({ progress }) => (
  <Box flexDirection="column" marginBottom={1}>
    {progress.toolCalls && progress.toolCalls.length > 0 && (
      <CleanToolCallsList toolCalls={progress.toolCalls} />
    )}
  </Box>
);

/**
 * Clean tool calls list - format consistent with ToolInfo in ToolMessage.tsx
 */
const CleanToolCallsList: React.FC<{
  toolCalls: Array<{
    name: string;
    status: 'executing' | 'success' | 'failed';
    error?: string;
    args?: Record<string, unknown>;
    result?: string;
    returnDisplay?: string;
  }>;
}> = ({ toolCalls }) => (
  <Box flexDirection="column" marginBottom={1}>
    <Box flexDirection="row" marginBottom={1}>
      <Text bold>Tools:</Text>
    </Box>
    {toolCalls.map((toolCall, index) => (
      <CleanToolCallItem
        key={`${toolCall.name}-${index}`}
        toolCall={toolCall}
      />
    ))}
  </Box>
);

/**
 * Individual tool call item - consistent with ToolInfo format
 */
const CleanToolCallItem: React.FC<{
  toolCall: {
    name: string;
    status: 'executing' | 'success' | 'failed';
    error?: string;
    args?: Record<string, unknown>;
    result?: string;
    returnDisplay?: string;
  };
}> = ({ toolCall }) => {
  const STATUS_INDICATOR_WIDTH = 3;

  // Map subagent status to ToolCallStatus-like display
  const statusIcon = React.useMemo(() => {
    switch (toolCall.status) {
      case 'executing':
        return <Text color={Colors.AccentYellow}>⊷</Text>; // Using same as ToolMessage
      case 'success':
        return <Text color={Colors.AccentGreen}>✔</Text>;
      case 'failed':
        return (
          <Text color={Colors.AccentRed} bold>
            x
          </Text>
        );
      default:
        return <Text color={Colors.Gray}>o</Text>;
    }
  }, [toolCall.status]);

  const description = getToolDescription(toolCall);

  // Get first line of returnDisplay for truncated output
  const truncatedOutput = React.useMemo(() => {
    if (!toolCall.returnDisplay) return '';
    const firstLine = toolCall.returnDisplay.split('\n')[0];
    return firstLine.length > 80
      ? firstLine.substring(0, 80) + '...'
      : firstLine;
  }, [toolCall.returnDisplay]);

  return (
    <Box flexDirection="column" paddingLeft={1} marginBottom={0}>
      {/* First line: status icon + tool name + description (consistent with ToolInfo) */}
      <Box flexDirection="row">
        <Box minWidth={STATUS_INDICATOR_WIDTH}>{statusIcon}</Box>
        <Text wrap="truncate-end">
          <Text color={Colors.Foreground} bold>
            {toolCall.name}
          </Text>{' '}
          <Text color={Colors.Gray}>{description}</Text>
          {toolCall.error && (
            <Text color={Colors.AccentRed}> - {toolCall.error}</Text>
          )}
        </Text>
      </Box>

      {/* Second line: truncated returnDisplay output */}
      {truncatedOutput && (
        <Box flexDirection="row" paddingLeft={STATUS_INDICATOR_WIDTH}>
          <Text color={Colors.Gray}>{truncatedOutput}</Text>
        </Box>
      )}
    </Box>
  );
};

/**
 * Helper function to get tool description from args
 */
const getToolDescription = (toolCall: {
  name: string;
  args?: Record<string, unknown>;
}): string => {
  if (!toolCall.args) return '';

  // Handle common tool patterns
  if (toolCall.name === 'Glob' && toolCall.args['glob_pattern']) {
    return `"${toolCall.args['glob_pattern']}"`;
  }
  if (toolCall.name === 'ReadFile' && toolCall.args['target_file']) {
    const path = toolCall.args['target_file'] as string;
    return path.split('/').pop() || path;
  }
  if (toolCall.name === 'SearchFileContent' && toolCall.args['pattern']) {
    return `"${toolCall.args['pattern']}"`;
  }

  // Generic fallback
  const firstArg = Object.values(toolCall.args)[0];
  if (typeof firstArg === 'string' && firstArg.length < 50) {
    return firstArg;
  }

  return '';
};

/**
 * Execution summary details component
 */
const ExecutionSummaryDetails: React.FC<{
  data: TaskResultDisplay;
}> = ({ data }) => {
  // Parse execution summary for structured data
  const summaryData = React.useMemo(() => {
    if (!data.executionSummary) return null;

    // Try to extract structured data from execution summary
    const durationMatch = data.executionSummary.match(/Duration:\s*([^\n]+)/i);
    const roundsMatch = data.executionSummary.match(/Rounds:\s*(\d+)/i);
    const tokensMatch = data.executionSummary.match(/Tokens:\s*([\d,]+)/i);

    return {
      duration: durationMatch?.[1] || 'N/A',
      rounds: roundsMatch?.[1] || 'N/A',
      tokens: tokensMatch?.[1] || 'N/A',
    };
  }, [data.executionSummary]);

  if (!summaryData) {
    return (
      <Box flexDirection="column" paddingLeft={1}>
        <Text color={Colors.Gray}>• No summary available</Text>
      </Box>
    );
  }

  return (
    <Box flexDirection="column" paddingLeft={1}>
      <Text>
        • <Text bold>Duration:</Text> {summaryData.duration}
      </Text>
      <Text>
        • <Text bold>Rounds:</Text> {summaryData.rounds}
      </Text>
      <Text>
        • <Text bold>Tokens:</Text> {summaryData.tokens}
      </Text>
    </Box>
  );
};

/**
 * Tool usage statistics component
 */
const ToolUsageStats: React.FC<{
  toolCalls: Array<{
    name: string;
    status: 'executing' | 'success' | 'failed';
    error?: string;
    args?: Record<string, unknown>;
    result?: string;
    returnDisplay?: string;
  }>;
}> = ({ toolCalls }) => {
  const stats = React.useMemo(() => {
    const total = toolCalls.length;
    const successful = toolCalls.filter(
      (call) => call.status === 'success',
    ).length;
    const failed = toolCalls.filter((call) => call.status === 'failed').length;
    const successRate =
      total > 0 ? ((successful / total) * 100).toFixed(1) : '0.0';

    return { total, successful, failed, successRate };
  }, [toolCalls]);

  return (
    <Box flexDirection="column" paddingLeft={1}>
      <Text>
        • <Text bold>Total Calls:</Text> {stats.total}
      </Text>
      <Text>
        • <Text bold>Success Rate:</Text>{' '}
        <Text color={Colors.AccentGreen}>{stats.successRate}%</Text> (
        <Text color={Colors.AccentGreen}>{stats.successful} success</Text>,{' '}
        <Text color={Colors.AccentRed}>{stats.failed} failed</Text>)
      </Text>
    </Box>
  );
};

/**
 * Results section for completed executions - matches the clean layout from the image
 */
const ResultsSection: React.FC<{
  data: TaskResultDisplay;
}> = ({ data }) => (
  <Box flexDirection="column">
    {/* Tool calls section - clean list format */}
    {data.progress?.toolCalls && data.progress.toolCalls.length > 0 && (
      <CleanToolCallsList toolCalls={data.progress.toolCalls} />
    )}

    {/* Task Completed section */}
    <Box flexDirection="row" marginTop={1} marginBottom={1}>
      <Text>📄 </Text>
      <Text bold>Task Completed: </Text>
      <Text>{data.taskDescription}</Text>
    </Box>

    {/* Execution Summary section */}
    <Box flexDirection="column" marginBottom={1}>
      <Box flexDirection="row" marginBottom={1}>
        <Text>📊 </Text>
        <Text bold color={Colors.AccentBlue}>
          Execution Summary:
        </Text>
      </Box>
      <ExecutionSummaryDetails data={data} />
    </Box>

    {/* Tool Usage section */}
    {data.progress?.toolCalls && data.progress.toolCalls.length > 0 && (
      <Box flexDirection="column">
        <Box flexDirection="row" marginBottom={1}>
          <Text>🔧 </Text>
          <Text bold color={Colors.AccentBlue}>
            Tool Usage:
          </Text>
        </Box>
        <ToolUsageStats toolCalls={data.progress.toolCalls} />
      </Box>
    )}

    {/* Error reason for failed tasks */}
    {data.status === 'failed' && data.terminateReason && (
      <Box flexDirection="row" marginTop={1}>
        <Text color={Colors.AccentRed}>❌ Failed: </Text>
        <Text color={Colors.Gray}>{data.terminateReason}</Text>
      </Box>
    )}
  </Box>
);
