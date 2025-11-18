# Qwen Code 认证流程说明

## 🔐 认证流程概览

```
用户打开 Chat UI
  ↓
WebViewProvider.show()
  ↓
检查 agentInitialized 标志
  ├─ 如果为 true → 跳过初始化（使用现有连接）
  └─ 如果为 false → 继续初始化
      ↓
      authStateManager.hasValidAuth()
      ├─ 有效缓存 → needsAuth = false
      └─ 无缓存/过期 → needsAuth = true
      ↓
      尝试恢复本地 session
      ├─ 成功 → sessionRestored = true, needsAuth = false
      └─ 失败 → 继续
      ↓
      如果 !sessionRestored && needsAuth
        ↓
        authenticate() (仅一次！) ✅
        ↓
        newSession()
        ↓
        saveAuthState()
      ↓
      agentInitialized = true
```

## ✅ 已修复的问题

### 问题 1: 嵌套 try-catch 导致重复认证（已修复）

**之前的代码**：

```typescript
try {
  if (switchSession fails) {
    authenticate(); // 第 1 次
  } else {
    authenticate(); // 第 1 次
  }
} catch {
  authenticate(); // 第 2 次！❌
}
```

**修复后的代码**：

```typescript
let needsAuth = true;
let sessionRestored = false;

// 检查缓存
if (hasValidAuth) {
  needsAuth = false;
}

// 尝试恢复 session
try {
  if (switchSession succeeds) {
    sessionRestored = true;
    needsAuth = false;
  }
} catch {
  // 只记录日志，不触发认证
}

// 只在必要时认证（最多一次）
if (!sessionRestored && needsAuth) {
  authenticate(); // 只会执行一次！✅
  newSession();
}
```

### 问题 2: agentInitialized 标志未重置（已修复）

**问题描述**：
清除认证缓存后，`agentInitialized` 标志仍为 `true`，导致不会重新初始化。

**修复方案**：

```typescript
// WebViewProvider.ts
resetAgentState(): void {
  this.agentInitialized = false;
  this.agentManager.disconnect();
}

// extension.ts
vscode.commands.registerCommand('qwenCode.clearAuthCache', async () => {
  await authStateManager.clearAuthState();
  webViewProvider.resetAgentState(); // ✅ 重置状态
});
```

## 🎯 正确的使用方式

### 场景 1: 正常使用（无需手动操作）

```
第1次打开 Chat UI:
  → 没有缓存
  → 需要登录 (1 次)
  → 保存缓存 (24小时)

第2次打开 Chat UI (24小时内):
  → 有缓存
  → 不需要登录 ✅

第3次打开 Chat UI (24小时后):
  → 缓存过期
  → 需要登录 (1 次)
  → 更新缓存
```

### 场景 2: 手动清除缓存

```
1. 执行命令: Qwen Code: Clear Authentication Cache
   → 清除缓存
   → 重置 agentInitialized 标志
   → 断开现有连接

2. 下次打开 Chat UI:
   → 没有缓存
   → 需要登录 (1 次) ✅
   → 保存新缓存
```

### 场景 3: 缓存有效但 token 失效

```
打开 Chat UI:
  → 缓存有效，跳过认证
  → 尝试创建 session
  → Session 创建失败（token 已过期）
  ↓
  【自动恢复】✅
  → 清除缓存
  → 重新认证 (1 次)
  → 保存新缓存
  → 重新创建 session
```

## ⚠️ 可能导致多次登录的情况

### 情况 1: Session 恢复失败 + 认证重试

如果 session 恢复失败，且认证也失败，会触发重试（最多 3 次）：

```
尝试恢复 session → 失败
  ↓
认证尝试 #1 → 失败
  ↓ (等待 1 秒)
认证尝试 #2 → 失败
  ↓ (等待 2 秒)
认证尝试 #3 → 失败
  ↓
抛出错误
```

**这是正常的重试机制**，用于处理网络临时故障。

### 情况 2: 多次打开/关闭 Chat UI

如果频繁打开关闭 Chat UI：

```
打开 #1 → 登录 → agentInitialized = true
关闭
打开 #2 → 使用现有连接 ✅ (不需要登录)
关闭
打开 #3 → 使用现有连接 ✅ (不需要登录)
```

**这是正常行为**，不会重复登录。

## 🐛 如何诊断"两次登录"问题

### 1. 查看详细日志

打开 VSCode 输出面板：

```
View → Output → 选择 "Qwen Code Companion"
```

查找以下关键日志：

#### 正常流程（只登录一次）：

```
[WebViewProvider] Starting initialization, workingDir: /path/to/workspace
[QwenAgentManager] Using cached authentication  ← 或者跳过这行（首次登录）
[QwenAgentManager] Creating new session...
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED at 2025-11-17T...  ← 只出现一次！
[QwenAgentManager] Call stack: ...
[QwenAgentManager] 📝 Authenticating (attempt 1/3)...
[QwenAgentManager] ✅ Authentication successful on attempt 1
[QwenAgentManager] New session created successfully
[AuthStateManager] Auth state saved
```

#### 异常流程（登录多次）：

```
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED at 2025-11-17T10:00:00  ← 第 1 次
[QwenAgentManager] Call stack: ...
[QwenAgentManager] ✅ Authentication successful on attempt 1
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED at 2025-11-17T10:00:05  ← 第 2 次！❌
[QwenAgentManager] Call stack: ...
```

**如果看到两个 "AUTHENTICATION CALL STARTED"，说明 `authenticateWithRetry()` 被调用了两次！**

### 2. 分析调用栈

查看每次认证调用的堆栈信息：

```
[QwenAgentManager] Call stack:
    at QwenAgentManager.authenticateWithRetry (/path/to/QwenAgentManager.ts:206)
    at QwenAgentManager.connect (/path/to/QwenAgentManager.ts:162)  ← 正常调用
    at WebViewProvider.show (/path/to/WebViewProvider.ts:131)
```

或者：

```
[QwenAgentManager] Call stack:
    at QwenAgentManager.authenticateWithRetry (/path/to/QwenAgentManager.ts:206)
    at QwenAgentManager.connect (/path/to/QwenAgentManager.ts:184)  ← 缓存失效重试！
    at WebViewProvider.show (/path/to/WebViewProvider.ts:131)
```

### 3. 区分"重试"和"重复调用"

**重要**：需要区分以下两种情况：

#### 情况 A: 认证重试（正常）

```
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED  ← 只有一次 CALL STARTED
[QwenAgentManager] 📝 Authenticating (attempt 1/3)...  ← 第 1 次尝试
[QwenAgentManager] ❌ Authentication attempt 1 failed
[QwenAgentManager] ⏳ Retrying in 1000ms...
[QwenAgentManager] 📝 Authenticating (attempt 2/3)...  ← 第 2 次尝试
[QwenAgentManager] ✅ Authentication successful on attempt 2
```

**这是正常的！** 这是同一个认证调用的多次尝试。

#### 情况 B: 重复认证调用（异常）

```
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED at ...  ← 第 1 个认证调用
[QwenAgentManager] 📝 Authenticating (attempt 1/3)...
[QwenAgentManager] ✅ Authentication successful on attempt 1
[QwenAgentManager] 🔐 AUTHENTICATION CALL STARTED at ...  ← 第 2 个认证调用！❌
[QwenAgentManager] 📝 Authenticating (attempt 1/3)...
```

**这是异常的！** `authenticateWithRetry()` 被调用了两次。

### 4. 检查 agentInitialized 标志

查找以下日志：

```
[WebViewProvider] Agent already initialized, reusing existing connection
```

如果每次打开都看到 "Starting initialization"，说明标志没有正确保持。

### 5. 检查是否是 OAuth 流程本身的问题

如果日志显示只有一次 "AUTHENTICATION CALL STARTED"，但浏览器弹出了两次授权页面，那可能是 **Qwen CLI 的 OAuth 流程本身需要两次交互**。

这种情况需要检查 Qwen CLI 的实现，不是 VSCode 扩展的问题。

## 🔧 手动测试步骤

### 测试 1: 正常流程

```bash
1. 清除缓存: Cmd+Shift+P → "Clear Authentication Cache"
2. 打开 Chat UI
3. 应该看到: 登录提示 (1 次)
4. 关闭 Chat UI
5. 重新打开 Chat UI
6. 应该看到: 直接连接，不需要登录 ✅
```

### 测试 2: 缓存过期

```bash
1. 修改 AUTH_CACHE_DURATION 为 1 分钟:
   // AuthStateManager.ts:21
   private static readonly AUTH_CACHE_DURATION = 1 * 60 * 1000;

2. 打开 Chat UI → 登录
3. 等待 2 分钟
4. 重新打开 Chat UI
5. 应该看到: 需要重新登录 (1 次) ✅
```

### 测试 3: 清除缓存

```bash
1. 打开 Chat UI (已登录)
2. 执行: "Clear Authentication Cache"
3. 关闭 Chat UI
4. 重新打开 Chat UI
5. 应该看到: 需要重新登录 (1 次) ✅
```

## 📊 认证状态管理

### 缓存存储位置

```
macOS:   ~/Library/Application Support/Code/User/globalStorage/
Linux:   ~/.config/Code/User/globalStorage/
Windows: %APPDATA%\Code\User\globalStorage\
```

### 缓存内容

```typescript
{
  isAuthenticated: true,
  authMethod: "qwen-oauth",  // 或 "openai"
  workingDir: "/path/to/workspace",
  timestamp: 1700000000000  // Unix timestamp
}
```

### 缓存有效期

- **默认**: 24 小时
- **修改方式**: 编辑 `AuthStateManager.ts:21`
- **检查方式**: 执行命令（如果添加了）或查看日志

## 🎯 关键代码位置

| 功能         | 文件                  | 行号    |
| ------------ | --------------------- | ------- |
| 认证缓存管理 | `AuthStateManager.ts` | 全文    |
| 认证逻辑     | `QwenAgentManager.ts` | 61-195  |
| 初始化控制   | `WebViewProvider.ts`  | 113-154 |
| 清除缓存命令 | `extension.ts`        | 148-160 |
| 缓存有效期   | `AuthStateManager.ts` | 21      |

## ✅ 总结

**当前实现已经修复了重复登录的问题**：

1. ✅ 使用 `needsAuth` 标志确保最多认证一次
2. ✅ 缓存有效时跳过认证
3. ✅ Session 恢复成功时跳过认证
4. ✅ 清除缓存时重置 `agentInitialized` 标志
5. ✅ 缓存失效时自动重新认证（只一次）

**如果仍然遇到多次登录**，请：

1. 检查日志确认是否真的登录了多次
2. 确认是否是重试机制（3 次尝试是正常的）
3. 检查是否多次打开了不同的 Chat UI 实例
4. 提供详细的日志帮助诊断

---

**最后更新**: 2025-11-17
