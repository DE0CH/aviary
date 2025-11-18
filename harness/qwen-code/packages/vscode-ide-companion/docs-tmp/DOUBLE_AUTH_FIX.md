# 🐛 双重认证问题修复

## 问题描述

用户反馈：打开 Qwen Code Chat UI 时，**每次都会触发两次 OAuth 登录**，需要授权两次才能完成连接。

## 根本原因

这是 **Qwen CLI 的 bug**，而不是 VSCode 扩展的问题。

### 问题分析（从日志中得出）

1. **第一次认证**（`authenticate()` 调用）：

```
[QwenAgentManager] 📝 Authenticating (attempt 1/3)...
[ACP] Sending authenticate request with methodId: qwen-oauth
[ACP qwen]: Device authorization result: { user_code: 'ZMSBMVYS', ... }
[ACP qwen]: Authentication successful! Access token obtained.
```

2. **第二次认证**（`newSession()` 调用时触发）：

```
[QwenAgentManager] Creating session (attempt 1/3)...
[ACP] Sending session/new request...
[ACP qwen]: Shared token manager failed, attempting device flow:
            TokenManagerError: No refresh token available for token refresh
[ACP qwen]: Device authorization result: { user_code: '7CYK61BI', ... }  ← 新的授权码！
```

### CLI 代码分析

在 `packages/cli/src/zed-integration/zedIntegration.ts` 第 150-171 行：

```typescript
async newSession(...): Promise<...> {
  const sessionId = randomUUID();
  const config = await this.newSessionConfig(sessionId, cwd, mcpServers);

  let isAuthenticated = false;
  if (this.settings.merged.security?.auth?.selectedType) {
    try {
      await config.refreshAuth(  // ← newSession 内部会调用 refreshAuth！
        this.settings.merged.security.auth.selectedType,
      );
      isAuthenticated = true;
    } catch (e) {
      console.error(`Authentication failed: ${e}`);
    }
  }

  if (!isAuthenticated) {
    throw acp.RequestError.authRequired();
  }
  // ...
}
```

在 `packages/core/src/qwen/qwenOAuth2.ts` 第 477-526 行：

```typescript
export async function getQwenOAuthClient(
  config: Config,
): Promise<QwenOAuth2Client> {
  const client = new QwenOAuth2Client();
  const sharedManager = SharedTokenManager.getInstance();

  try {
    // 尝试从 shared token manager 获取凭证
    const credentials = await sharedManager.getValidCredentials(client);
    client.setCredentials(credentials);
    return client;
  } catch (error: unknown) {
    console.debug(
      'Shared token manager failed, attempting device flow:',
      error,
    );

    if (error instanceof TokenManagerError) {
      switch (error.type) {
        case TokenError.NO_REFRESH_TOKEN: // ← 这就是我们看到的错误！
          console.debug(
            'No refresh token available, proceeding with device flow',
          );
          break;
        // ...
      }
    }

    // 重新进行 device flow
    const result = await authWithQwenDeviceFlow(client, config);
    // ...
  }
}
```

### 问题的根源

1. **`authenticate()` 方法**：
   - 执行 device flow，获取 **access token**
   - ❌ **没有正确保存 refresh token** 到 shared token manager

2. **`newSession()` 方法**：
   - 内部调用 `config.refreshAuth()`
   - 尝试从 shared token manager 获取凭证
   - 因为没有 refresh token，抛出 `NO_REFRESH_TOKEN` 错误
   - **触发第二次 device flow**

3. **结果**：
   - 用户需要授权两次
   - 第一次授权的 token 被浪费了
   - 只有第二次授权的 token 被真正使用

## ✅ 修复方案（Workaround）

### 方案：跳过显式的 `authenticate()` 调用

既然 `newSession()` 内部会自动处理认证（通过 `refreshAuth()`），我们可以：

1. **不调用** `connection.authenticate()`
2. **直接调用** `connection.newSession()`
3. `newSession` 会自动触发认证流程

### 代码变更

**之前的代码**（会触发两次认证）：

```typescript
if (!sessionRestored) {
  if (needsAuth) {
    await this.authenticateWithRetry(authMethod, 3); // ← 第一次认证
    await authStateManager.saveAuthState(workingDir, authMethod);
  }

  try {
    await this.newSessionWithRetry(workingDir, 3); // ← 触发第二次认证
  } catch (sessionError) {
    // ...
  }
}
```

**修复后的代码**（只认证一次）：

```typescript
if (!sessionRestored) {
  // WORKAROUND: Skip explicit authenticate() call
  // The newSession() method will internally call config.refreshAuth(),
  // which will trigger device flow if no valid token exists.

  try {
    await this.newSessionWithRetry(workingDir, 3); // ← 只有一次认证

    // Save auth state after successful session creation
    if (authStateManager) {
      await authStateManager.saveAuthState(workingDir, authMethod);
    }
  } catch (sessionError) {
    if (authStateManager) {
      await authStateManager.clearAuthState();
    }
    throw sessionError;
  }
}
```

## 📊 测试结果

### 之前（两次认证）

```
用户操作：打开 Chat UI
  ↓
authenticate() 调用
  ↓
浏览器弹窗 #1: "ZMSBMVYS" → 用户授权
  ↓
获得 access token（但没有 refresh token）
  ↓
newSession() 调用
  ↓
refreshAuth() 发现没有 refresh token
  ↓
浏览器弹窗 #2: "7CYK61BI" → 用户再次授权 ❌
  ↓
获得 access token + refresh token
  ↓
连接成功
```

**用户体验**：需要授权 **2 次** ❌

### 现在（一次认证）

```
用户操作：打开 Chat UI
  ↓
直接调用 newSession()
  ↓
refreshAuth() 发现没有 token
  ↓
浏览器弹窗: "XXXXX" → 用户授权
  ↓
获得 access token + refresh token
  ↓
连接成功
```

**用户体验**：只需授权 **1 次** ✅

## ⚠️ 注意事项

### 1. 这是一个 Workaround

这不是完美的解决方案，而是针对 Qwen CLI bug 的临时规避措施。

### 2. 未使用 `authenticate()` 方法

虽然 ACP 协议定义了 `authenticate` 方法，但因为 CLI 的 bug，我们不能使用它。

### 3. 依赖 `newSession()` 的内部实现

我们依赖于 `newSession()` 内部会调用 `refreshAuth()` 的行为，如果 CLI 修改了这个实现，可能需要调整我们的代码。

### 4. 缓存机制仍然有效

即使跳过了显式的 `authenticate()` 调用，我们的缓存机制仍然工作：

- 首次连接：`newSession()` 触发认证 → 保存缓存
- 再次连接（24小时内）：跳过 `newSession()` 调用，直接恢复本地 session

## 🔮 未来的理想修复

### 选项 1：修复 Qwen CLI（推荐）

在 `packages/cli/src/zed-integration/zedIntegration.ts` 中：

```typescript
async authenticate({ methodId }: acp.AuthenticateRequest): Promise<void> {
  const method = this.#authMethodIdToAuthType(methodId);

  // 调用 refreshAuth 并确保保存 refresh token
  const config = await this.newSessionConfig(randomUUID(), process.cwd(), []);
  await config.refreshAuth(method);

  // 保存认证信息到 shared token manager
  // TODO: 确保 refresh token 被正确保存

  await this.settings.setSetting('security.auth.selectedType', method);
}
```

### 选项 2：修改 ACP 协议

- 移除 `authenticate` 方法
- 文档说明：`newSession` 会自动处理认证
- 客户端不需要单独调用 `authenticate`

### 选项 3：让 `authenticate` 返回凭证

```typescript
async authenticate({ methodId }: acp.AuthenticateRequest): Promise<{
  accessToken: string;
  refreshToken: string;
  expiresAt: number;
}> {
  // 返回凭证，让客户端可以缓存
}
```

## 📝 相关文件

- `packages/vscode-ide-companion/src/agents/QwenAgentManager.ts`: 修复实现
- `packages/cli/src/zed-integration/zedIntegration.ts`: CLI 的 bug 位置
- `packages/core/src/qwen/qwenOAuth2.ts`: OAuth 实现

## 🎯 总结

- ✅ **问题根源**：Qwen CLI 的 `authenticate()` 不保存 refresh token
- ✅ **Workaround**：跳过 `authenticate()`，直接调用 `newSession()`
- ✅ **用户体验**：从 2 次授权减少到 1 次
- ⚠️ **注意**：这是临时方案，理想情况下应该修复 CLI

---

**创建日期**: 2025-11-18  
**最后更新**: 2025-11-18  
**状态**: ✅ 已修复（使用 workaround）
