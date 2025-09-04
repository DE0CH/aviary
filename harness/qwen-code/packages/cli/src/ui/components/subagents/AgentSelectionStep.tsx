/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { useEffect, useState } from 'react';
import { Box, Text } from 'ink';
import { ManagementStepProps } from './types.js';
import { theme } from '../../semantic-colors.js';
import { Colors } from '../../colors.js';
import { useKeypress } from '../../hooks/useKeypress.js';

interface NavigationState {
  currentBlock: 'project' | 'user';
  projectIndex: number;
  userIndex: number;
}

export const AgentSelectionStep = ({
  state,
  dispatch,
  onNext,
  config,
}: ManagementStepProps) => {
  const [isLoading, setIsLoading] = useState(false);
  const [navigation, setNavigation] = useState<NavigationState>({
    currentBlock: 'project',
    projectIndex: 0,
    userIndex: 0,
  });

  // Group agents by level
  const projectAgents = state.availableAgents.filter(
    (agent) => agent.level === 'project',
  );
  const userAgents = state.availableAgents.filter(
    (agent) => agent.level === 'user',
  );
  const projectNames = new Set(projectAgents.map((agent) => agent.name));

  useEffect(() => {
    const loadAgents = async () => {
      setIsLoading(true);
      dispatch({ type: 'SET_LOADING', payload: true });

      try {
        if (!config) {
          throw new Error('Configuration not available');
        }
        const manager = config.getSubagentManager();

        // Load agents from both levels separately to show all agents including conflicts
        const [projectAgents, userAgents] = await Promise.all([
          manager.listSubagents({ level: 'project' }),
          manager.listSubagents({ level: 'user' }),
        ]);

        // Combine all agents (project and user level)
        const allAgents = [...projectAgents, ...userAgents];

        dispatch({ type: 'SET_AVAILABLE_AGENTS', payload: allAgents });
        dispatch({ type: 'SET_ERROR', payload: null });
      } catch (error) {
        const errorMessage =
          error instanceof Error ? error.message : 'Unknown error';
        dispatch({
          type: 'SET_ERROR',
          payload: `Failed to load agents: ${errorMessage}`,
        });
      } finally {
        setIsLoading(false);
        dispatch({ type: 'SET_LOADING', payload: false });
      }
    };

    loadAgents();
  }, [dispatch, config]);

  // Initialize navigation state when agents are loaded
  useEffect(() => {
    if (projectAgents.length > 0) {
      setNavigation((prev) => ({ ...prev, currentBlock: 'project' }));
    } else if (userAgents.length > 0) {
      setNavigation((prev) => ({ ...prev, currentBlock: 'user' }));
    }
  }, [projectAgents.length, userAgents.length]);

  // Custom keyboard navigation
  useKeypress(
    (key) => {
      const { name } = key;

      if (name === 'up' || name === 'k') {
        setNavigation((prev) => {
          if (prev.currentBlock === 'project') {
            if (prev.projectIndex > 0) {
              return { ...prev, projectIndex: prev.projectIndex - 1 };
            } else if (userAgents.length > 0) {
              // Move to last item in user block
              return {
                ...prev,
                currentBlock: 'user',
                userIndex: userAgents.length - 1,
              };
            } else {
              // Wrap to last item in project block
              return { ...prev, projectIndex: projectAgents.length - 1 };
            }
          } else {
            if (prev.userIndex > 0) {
              return { ...prev, userIndex: prev.userIndex - 1 };
            } else if (projectAgents.length > 0) {
              // Move to last item in project block
              return {
                ...prev,
                currentBlock: 'project',
                projectIndex: projectAgents.length - 1,
              };
            } else {
              // Wrap to last item in user block
              return { ...prev, userIndex: userAgents.length - 1 };
            }
          }
        });
      } else if (name === 'down' || name === 'j') {
        setNavigation((prev) => {
          if (prev.currentBlock === 'project') {
            if (prev.projectIndex < projectAgents.length - 1) {
              return { ...prev, projectIndex: prev.projectIndex + 1 };
            } else if (userAgents.length > 0) {
              // Move to first item in user block
              return { ...prev, currentBlock: 'user', userIndex: 0 };
            } else {
              // Wrap to first item in project block
              return { ...prev, projectIndex: 0 };
            }
          } else {
            if (prev.userIndex < userAgents.length - 1) {
              return { ...prev, userIndex: prev.userIndex + 1 };
            } else if (projectAgents.length > 0) {
              // Move to first item in project block
              return { ...prev, currentBlock: 'project', projectIndex: 0 };
            } else {
              // Wrap to first item in user block
              return { ...prev, userIndex: 0 };
            }
          }
        });
      } else if (name === 'return' || name === 'space') {
        // Select current item
        const currentAgent =
          navigation.currentBlock === 'project'
            ? projectAgents[navigation.projectIndex]
            : userAgents[navigation.userIndex];

        if (currentAgent) {
          const agentIndex = state.availableAgents.indexOf(currentAgent);
          handleAgentSelect(agentIndex);
        }
      }
    },
    { isActive: true },
  );

  const handleAgentSelect = async (index: number) => {
    const selectedMetadata = state.availableAgents[index];
    if (!selectedMetadata) return;

    try {
      if (!config) {
        throw new Error('Configuration not available');
      }
      const manager = config.getSubagentManager();
      const agent = await manager.loadSubagent(
        selectedMetadata.name,
        selectedMetadata.level,
      );

      if (agent) {
        dispatch({ type: 'SELECT_AGENT', payload: { agent, index } });
        onNext();
      } else {
        dispatch({
          type: 'SET_ERROR',
          payload: `Failed to load agent: ${selectedMetadata.name}`,
        });
      }
    } catch (error) {
      const errorMessage =
        error instanceof Error ? error.message : 'Unknown error';
      dispatch({
        type: 'SET_ERROR',
        payload: `Failed to load agent: ${errorMessage}`,
      });
    }
  };

  if (isLoading) {
    return (
      <Box>
        <Text color={theme.text.secondary}>Loading agents...</Text>
      </Box>
    );
  }

  if (state.error) {
    return (
      <Box>
        <Text color={theme.status.error}>{state.error}</Text>
      </Box>
    );
  }

  if (state.availableAgents.length === 0) {
    return (
      <Box flexDirection="column">
        <Text color={theme.text.secondary}>No subagents found.</Text>
        <Text color={theme.text.secondary}>
          Use &apos;/agents create&apos; to create your first subagent.
        </Text>
      </Box>
    );
  }

  // Render custom radio button items
  const renderAgentItem = (
    agent: { name: string; level: 'project' | 'user' },
    index: number,
    isSelected: boolean,
  ) => {
    const textColor = isSelected ? theme.text.accent : theme.text.primary;

    return (
      <Box key={agent.name} alignItems="center">
        <Box minWidth={2} flexShrink={0}>
          <Text color={isSelected ? theme.text.accent : theme.text.primary}>
            {isSelected ? '●' : ' '}
          </Text>
        </Box>
        <Text color={textColor} wrap="truncate">
          {agent.name}
          {agent.level === 'user' && projectNames.has(agent.name) && (
            <Text color={isSelected ? theme.status.warning : Colors.Gray}>
              {' '}
              (overridden by project level agent)
            </Text>
          )}
        </Text>
      </Box>
    );
  };

  // Calculate enabled agents count (excluding conflicted user-level agents)
  const enabledAgentsCount =
    projectAgents.length +
    userAgents.filter((agent) => !projectNames.has(agent.name)).length;

  return (
    <Box flexDirection="column">
      {/* Project Level Agents */}
      {projectAgents.length > 0 && (
        <Box flexDirection="column" marginBottom={1}>
          <Text color={theme.text.primary} bold>
            Project Level ({projectAgents[0].filePath.replace(/\/[^/]+$/, '')})
          </Text>
          <Box marginTop={1} flexDirection="column">
            {projectAgents.map((agent, index) => {
              const isSelected =
                navigation.currentBlock === 'project' &&
                navigation.projectIndex === index;
              return renderAgentItem(agent, index, isSelected);
            })}
          </Box>
        </Box>
      )}

      {/* User Level Agents */}
      {userAgents.length > 0 && (
        <Box flexDirection="column">
          <Text color={theme.text.primary} bold>
            User Level ({userAgents[0].filePath.replace(/\/[^/]+$/, '')})
          </Text>
          <Box marginTop={1} flexDirection="column">
            {userAgents.map((agent, index) => {
              const isSelected =
                navigation.currentBlock === 'user' &&
                navigation.userIndex === index;
              return renderAgentItem(agent, index, isSelected);
            })}
          </Box>
        </Box>
      )}

      {/* Agent count summary */}
      {(projectAgents.length > 0 || userAgents.length > 0) && (
        <Box marginTop={1}>
          <Text color={theme.text.secondary}>
            Using: {enabledAgentsCount} agents
          </Text>
        </Box>
      )}
    </Box>
  );
};
