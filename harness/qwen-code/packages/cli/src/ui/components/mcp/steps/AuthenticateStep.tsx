/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { useState } from 'react';
import { Box, Text } from 'ink';
import { theme } from '../../../semantic-colors.js';
import { useKeypress } from '../../../hooks/useKeypress.js';
import { RadioButtonSelect } from '../../shared/RadioButtonSelect.js';
import { t } from '../../../../i18n/index.js';
import type { AuthenticateStepProps } from '../types.js';
import type { MCPServerConfig } from '@qwen-code/qwen-code-core';

// TODO: 稍后从 utils.ts 导入此函数
const getOAuthConfigFromServerConfig = (_config: MCPServerConfig): unknown =>
  null;

type AuthAction = 'authenticate' | 'back';

export const AuthenticateStep: React.FC<AuthenticateStepProps> = ({
  server,
  onBack,
}) => {
  const [selectedAction, setSelectedAction] = useState<AuthAction>('back');
  const [isAuthenticating, setIsAuthenticating] = useState(false);
  const [authError, setAuthError] = useState<string | null>(null);

  const actions = [
    {
      key: 'authenticate',
      label: t('Authenticate'),
      value: 'authenticate' as const,
    },
    {
      key: 'back',
      label: t('Back'),
      value: 'back' as const,
    },
  ];

  useKeypress(
    (key) => {
      if (key.name === 'escape') {
        onBack();
      } else if (key.name === 'return' && !isAuthenticating) {
        switch (selectedAction) {
          case 'authenticate':
            handleAuthenticate();
            break;
          case 'back':
            onBack();
            break;
          default:
            break;
        }
      }
    },
    { isActive: true },
  );

  const handleAuthenticate = async () => {
    if (!server) return;

    setIsAuthenticating(true);
    setAuthError(null);

    try {
      // TODO: 实现 OAuth 认证逻辑
      // 这里需要调用 MCPOAuthProvider 进行认证
      // 认证成功后调用 onSuccess()
      // 认证失败时设置 authError

      // 临时实现：显示提示信息
      setAuthError(t('OAuth authentication is not yet implemented'));
    } catch (error) {
      setAuthError(
        error instanceof Error ? error.message : t('Authentication failed'),
      );
    } finally {
      setIsAuthenticating(false);
    }
  };

  if (!server) {
    return (
      <Box>
        <Text color={theme.status.error}>{t('No server selected')}</Text>
      </Box>
    );
  }

  const oauthConfig = getOAuthConfigFromServerConfig(server.config);
  const hasOAuth = !!oauthConfig;

  return (
    <Box flexDirection="column" gap={1}>
      {/* 认证说明 */}
      <Box flexDirection="column">
        <Text color={theme.text.primary} bold>
          {t('OAuth Authentication')}
        </Text>

        <Box marginTop={1}>
          <Text color={theme.text.secondary}>
            {t('Server:')} {server.name}
          </Text>
        </Box>

        {!hasOAuth && (
          <Box marginTop={1}>
            <Text color={theme.status.warning}>
              {t('This server does not have OAuth configuration.')}
            </Text>
          </Box>
        )}

        {authError && (
          <Box marginTop={1}>
            <Text color={theme.status.error}>{authError}</Text>
          </Box>
        )}
      </Box>

      {/* 操作列表 */}
      {!hasOAuth ? (
        <Box>
          <RadioButtonSelect<AuthAction>
            items={actions.filter((a) => a.key === 'back')}
            onHighlight={(value: AuthAction) => setSelectedAction(value)}
            onSelect={(value: AuthAction) => {
              if (value === 'back') {
                onBack();
              }
            }}
          />
        </Box>
      ) : (
        <Box>
          <RadioButtonSelect<AuthAction>
            items={actions}
            onHighlight={(value: AuthAction) => setSelectedAction(value)}
            onSelect={(value: AuthAction) => {
              if (value === 'back') {
                onBack();
              }
            }}
          />
        </Box>
      )}

      {isAuthenticating && (
        <Box>
          <Text color={theme.text.secondary}>
            {t('Authenticating... Please wait.')}
          </Text>
        </Box>
      )}
    </Box>
  );
};
