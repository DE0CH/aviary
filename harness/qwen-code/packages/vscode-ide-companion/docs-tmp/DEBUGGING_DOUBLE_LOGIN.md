# 🐛 调试"两次登录"问题指南

## 问题描述

用户反馈：打开 Qwen Code Chat UI 时，似乎一定会触发两次登录。

## 已实施的修复

### 1. ✅ 添加详细日志追踪

在 `QwenAgentManager.ts` 的 `authenticateWithRetry` 方法中添加了详细日志：

```typescript
private async authenticateWithRetry(authMethod: string, maxRetries: number) {
  const timestamp = new Date().toISOString();
  const callStack = new Error().stack;
  console.log(`[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED at ${timestamp}`);
  console.log(`[QwenAgentManager] Auth method: ${authMethod}, Max retries: ${maxRetries}`);
  console.log(`[QwenAgentManager] Call stack:\n${callStack}`);

  // ... 认证逻辑
}
```

**作用**：每次调用 `authenticateWithRetry` 都会打印时间戳和调用栈，方便追踪是否有重复调用。

### 2. ✅ 修复 agentInitialized 标志未重置问题

在 `WebViewProvider.ts` 中添加了 `resetAgentState` 方法：

```typescript
resetAgentState(): void {
  console.log('[WebViewProvider] Resetting agent state');
  this.agentInitialized = false;
  this.agentManager.disconnect();
}
```

在 `extension.ts` 的 `clearAuthCache` 命令中调用：

```typescript
vscode.commands.registerCommand('qwenCode.clearAuthCache', async () => {
  await authStateManager.clearAuthState();

  // Reset WebView agent state to force re-authentication
  if (webViewProvider) {
    webViewProvider.resetAgentState();
  }

  vscode.window.showInformationMessage(
    'Qwen Code authentication cache cleared. You will need to login again on next connection.',
  );
});
```

**作用**：确保清除缓存后，下次打开 Chat UI 会正确地重新初始化和认证。

## 🔍 如何测试和诊断

### 步骤 1: 清除现有缓存

```bash
# 在 VSCode 中执行命令
Cmd+Shift+P (macOS) 或 Ctrl+Shift+P (Windows/Linux)
输入: "Qwen Code: Clear Authentication Cache"
```

### 步骤 2: 打开输出面板

```bash
View → Output → 选择 "Qwen Code Companion"
```

### 步骤 3: 打开 Chat UI 并观察日志

```bash
Cmd+Shift+P → "Qwen Code: Open Chat"
```

### 步骤 4: 分析日志

#### ✅ 正常情况（只认证一次）

```
[WebViewProvider] Starting initialization, workingDir: /Users/xxx/workspace
[WebViewProvider] Connecting to agent...
[QwenAgentManager] Reading local session files...
[QwenAgentManager] Creating new session...
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED at 2025-11-17T10:00:00.000Z
[QwenAgentManager] Auth method: qwen-oauth, Max retries: 3
[QwenAgentManager] Call stack:
    at QwenAgentManager.authenticateWithRetry (/path/to/QwenAgentManager.ts:206)
    at QwenAgentManager.connect (/path/to/QwenAgentManager.ts:162)
    ...
[QwenAgentManager] 📝 Authenticating (attempt 1/3)...
[ACP] Sending authenticate request with methodId: qwen-oauth
[ACP] Authenticate successful
[QwenAgentManager] ✅ Authentication successful on attempt 1
[QwenAgentManager] New session created successfully
[AuthStateManager] Auth state saved
[WebViewProvider] Agent connected successfully
```

**关键点**：

- 只有 **一个** "🔐 AUTHENTICATION CALL STARTED"
- 认证成功后立即创建 session
- 整个流程顺利完成

#### ❌ 异常情况（认证多次）

```
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED at 2025-11-17T10:00:00.000Z
[QwenAgentManager] Call stack:
    at QwenAgentManager.authenticateWithRetry (/path/to/QwenAgentManager.ts:206)
    at QwenAgentManager.connect (/path/to/QwenAgentManager.ts:162)  ← 第一次调用
[QwenAgentManager] 📝 Authenticating (attempt 1/3)...
[QwenAgentManager] ✅ Authentication successful on attempt 1
[QwenAgentManager] Session creation failed...
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED at 2025-11-17T10:00:05.000Z
[QwenAgentManager] Call stack:
    at QwenAgentManager.authenticateWithRetry (/path/to/QwenAgentManager.ts:206)
    at QwenAgentManager.connect (/path/to/QwenAgentManager.ts:184)  ← 第二次调用（缓存失效重试）
[QwenAgentManager] 📝 Authenticating (attempt 1/3)...
```

**关键点**：

- 有 **两个** "🔐 AUTHENTICATION CALL STARTED"
- 第一次认证成功
- Session 创建失败
- 触发了缓存失效重试（第 184 行）

## 🤔 可能的原因分析

### 情况 1: 缓存有效但 Token 过期

**触发条件**：

- 本地缓存显示已认证
- 跳过认证，直接创建 session
- Session 创建失败（因为服务器端 token 已过期）
- 触发缓存失效逻辑，清除缓存并重新认证

**代码位置**：
`QwenAgentManager.ts:173-194`

```typescript
try {
  await this.newSessionWithRetry(workingDir, 3);
} catch (sessionError) {
  // 如果使用了缓存（needsAuth = false），但 session 创建失败
  if (!needsAuth && authStateManager) {
    console.log(
      '[QwenAgentManager] Session creation failed with cached auth...',
    );
    await authStateManager.clearAuthState();

    // 第二次认证！
    await this.authenticateWithRetry(authMethod, 3);
    await authStateManager.saveAuthState(workingDir, authMethod);
    await this.newSessionWithRetry(workingDir, 3);
  }
}
```

**是否正常**：

- ✅ **这是正常的设计**，用于处理缓存有效但服务器 token 过期的情况
- ⚠️ 但如果 **每次** 都触发这个逻辑，说明有问题

### 情况 2: 认证重试（单次调用的多次尝试）

**触发条件**：

- 网络不稳定
- 认证失败，自动重试

**日志特征**：

```
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED  ← 只有一个
[QwenAgentManager] 📝 Authenticating (attempt 1/3)...
[QwenAgentManager] ❌ Authentication attempt 1 failed
[QwenAgentManager] ⏳ Retrying in 1000ms...
[QwenAgentManager] 📝 Authenticating (attempt 2/3)...
[QwenAgentManager] ✅ Authentication successful on attempt 2
```

**是否正常**：

- ✅ **这是正常的**，这是单次认证调用的重试机制
- 用户可能看到多次浏览器弹窗，但这是预期行为

### 情况 3: OAuth 流程本身需要多次交互

**可能原因**：

- Qwen CLI 的 OAuth 实现可能需要：
  1. 第一步：获取授权码（authorization code）
  2. 第二步：交换访问令牌（access token）

**如何确认**：

- 如果日志只显示 **一个** "🔐 AUTHENTICATION CALL STARTED"
- 但浏览器弹出了 **两次** 授权页面
- 那么是 CLI 的 OAuth 流程设计，不是扩展的问题

### 情况 4: agentInitialized 标志失效

**触发条件**：

- 每次打开 Chat UI 都重新初始化
- `agentInitialized` 没有正确保持为 `true`

**如何确认**：

```
第1次打开:
[WebViewProvider] Starting initialization...  ← 正常

第2次打开（不关闭 WebView）:
[WebViewProvider] Starting initialization...  ← 异常！应该是 "Agent already initialized"
```

**修复状态**：

- ✅ 已修复（添加了 `resetAgentState` 方法）

## 📋 测试清单

请按照以下步骤测试，并记录日志：

### ✅ 测试 1: 首次登录

```bash
1. 清除缓存: Cmd+Shift+P → "Clear Authentication Cache"
2. 重启 VSCode Extension Host (F5 重新调试)
3. 打开 Chat UI: Cmd+Shift+P → "Open Chat"
4. 记录日志中 "🔐 AUTHENTICATION CALL STARTED" 出现的次数
```

**期望结果**：

- "🔐 AUTHENTICATION CALL STARTED" 只出现 **1 次**
- 浏览器弹窗次数：取决于 OAuth 流程（1-2 次都是正常的）

### ✅ 测试 2: 缓存有效时

```bash
1. 首次登录成功（参考测试 1）
2. 关闭 Chat UI WebView
3. 重新打开 Chat UI
4. 检查日志
```

**期望结果**：

- 看到 "Using cached authentication"
- **不应该** 出现 "🔐 AUTHENTICATION CALL STARTED"
- 不需要重新登录

### ✅ 测试 3: 清除缓存后重新登录

```bash
1. 打开 Chat UI（已登录状态）
2. 执行: Cmd+Shift+P → "Clear Authentication Cache"
3. 检查日志是否有 "Resetting agent state"
4. 关闭 Chat UI
5. 重新打开 Chat UI
6. 检查是否需要重新登录
```

**期望结果**：

- 清除缓存后看到 "Resetting agent state"
- 重新打开时需要登录
- "🔐 AUTHENTICATION CALL STARTED" 只出现 **1 次**

### ✅ 测试 4: agentInitialized 标志

```bash
1. 打开 Chat UI（首次）
2. 不要关闭 WebView
3. 再次执行: Cmd+Shift+P → "Open Chat"
4. 检查日志
```

**期望结果**：

- 第二次打开时看到 "Agent already initialized, reusing existing connection"
- **不应该** 重新初始化

## 🎯 下一步行动

### 如果测试失败（仍然两次登录）

1. **收集完整日志**：
   - 从 Extension Host 启动开始
   - 到第二次 "AUTHENTICATION CALL STARTED" 结束
   - 包括完整的调用栈信息

2. **检查调用栈**：
   - 确认两次调用是从哪里触发的
   - 第一次：应该是 line 162 (needsAuth 分支)
   - 第二次：应该是 line 184 (缓存失效重试)

3. **确认是否是"情况 1"**：
   - 如果确实是缓存失效导致的第二次认证
   - 需要调查为什么 session 创建会失败

4. **可能的解决方案**：
   - 如果是缓存时间太长，可以缩短 AUTH_CACHE_DURATION
   - 如果是 session 创建逻辑有问题，需要修复 newSessionWithRetry
   - 如果是 CLI 的问题，需要向 Qwen Code 团队反馈

### 如果是 OAuth 流程本身的问题

- 联系 Qwen Code 团队
- 确认是否可以优化 OAuth 流程，减少用户交互次数
- 或者在文档中说明这是正常行为

---

**创建日期**: 2025-11-17  
**最后更新**: 2025-11-17  
**负责人**: VSCode Extension Team
