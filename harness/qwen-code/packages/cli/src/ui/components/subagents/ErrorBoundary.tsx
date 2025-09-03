/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import React, { Component, ReactNode } from 'react';
import { Box, Text } from 'ink';

interface ErrorBoundaryState {
  hasError: boolean;
  error?: Error;
  errorInfo?: React.ErrorInfo;
}

interface ErrorBoundaryProps {
  children: ReactNode;
  fallback?: ReactNode;
  onError?: (error: Error, errorInfo: React.ErrorInfo) => void;
}

/**
 * Error boundary component for graceful error handling in the subagent wizard.
 */
export class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  constructor(props: ErrorBoundaryProps) {
    super(props);
    this.state = { hasError: false };
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return {
      hasError: true,
      error,
    };
  }

  override componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    this.setState({
      error,
      errorInfo,
    });

    // Call optional error handler
    this.props.onError?.(error, errorInfo);

    // Log error for debugging
    console.error(
      'SubagentWizard Error Boundary caught an error:',
      error,
      errorInfo,
    );
  }

  override render() {
    if (this.state.hasError) {
      // Custom fallback UI or default error display
      if (this.props.fallback) {
        return this.props.fallback;
      }

      return (
        <Box flexDirection="column" gap={1}>
          <Box>
            <Text bold color="red">
              ❌ Subagent Wizard Error
            </Text>
          </Box>

          <Box>
            <Text>
              An unexpected error occurred in the subagent creation wizard.
            </Text>
          </Box>

          {this.state.error && (
            <Box flexDirection="column" marginTop={1}>
              <Text bold color="yellow">
                Error Details:
              </Text>
              <Text color="gray" wrap="wrap">
                {this.state.error.message}
              </Text>
            </Box>
          )}

          <Box marginTop={1}>
            <Text color="gray">
              Press <Text color="cyan">Esc</Text> to close the wizard and try
              again.
            </Text>
          </Box>

          {process.env['NODE_ENV'] === 'development' &&
            this.state.errorInfo && (
              <Box flexDirection="column" marginTop={1}>
                <Text bold color="yellow">
                  Stack Trace (Development):
                </Text>
                <Text color="gray" wrap="wrap">
                  {this.state.errorInfo.componentStack}
                </Text>
              </Box>
            )}
        </Box>
      );
    }

    return this.props.children;
  }
}
