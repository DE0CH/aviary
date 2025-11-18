# 从 Claude Code 压缩代码中提取的可用逻辑

> **核心方法**: 通过字符串锚点和 HTML 结构反推 React 组件逻辑
>
> **日期**: 2025-11-18

---

## 一、成功提取的组件结构

### 1. Header 组件的 React 代码模式

#### HTML 结构分析

```html
<div class="he">
  <!-- Session 下拉按钮 -->
  <button class="E" title="Past conversations">
    <span class="xe">
      <span class="fe">Past Conversations</span>
      <svg class="we"><!-- 下拉箭头 --></svg>
    </span>
  </button>

  <!-- Spacer -->
  <div class="ke"></div>

  <!-- 新建按钮 -->
  <button title="New Session" class="j">
    <svg class="we"><!-- Plus icon --></svg>
  </button>
</div>
```

#### 推断的 React 代码

```typescript
// 从混淆代码中找到的模式
import React from 'react';

interface HeaderProps {
  currentSessionTitle: string;
  onSessionsClick: () => void;
  onNewSessionClick: () => void;
}

const ChatHeader: React.FC<HeaderProps> = ({
  currentSessionTitle,
  onSessionsClick,
  onNewSessionClick
}) => {
  return (
    <div className="chat-header">
      {/* Session Dropdown Button */}
      <button
        className="session-dropdown-button"
        title="Past conversations"
        onClick={onSessionsClick}
      >
        <span className="session-dropdown-content">
          <span className="session-title">
            {currentSessionTitle || "Past Conversations"}
          </span>
          <svg className="dropdown-icon" viewBox="0 0 20 20">
            <path fillRule="evenodd" d="M5.22 8.22a.75.75 0 0 1 1.06 0L10 11.94l3.72-3.72a.75.75 0 1 1 1.06 1.06l-4.25 4.25a.75.75 0 0 1-1.06 0L5.22 9.28a.75.75 0 0 1 0-1.06Z" clipRule="evenodd" />
          </svg>
        </span>
      </button>

      {/* Spacer */}
      <div className="header-spacer"></div>

      {/* New Session Button */}
      <button
        title="New Session"
        className="new-session-button"
        onClick={onNewSessionClick}
      >
        <svg className="plus-icon" viewBox="0 0 20 20">
          <path d="M10.75 4.75a.75.75 0 0 0-1.5 0v4.5h-4.5a.75.75 0 0 0 0 1.5h4.5v4.5a.75.75 0 0 0 1.5 0v-4.5h4.5a.75.75 0 0 0 0-1.5h-4.5v-4.5Z" />
        </svg>
      </button>
    </div>
  );
};

export default ChatHeader;
```

**关键发现**:

- ✅ 使用 `title` 属性提示用户
- ✅ SVG 图标直接内联
- ✅ `className` 使用单一类名(我们可以改进为多个)

---

### 2. 输入框组件模式

#### HTML 结构

```html
<form class="u" data-permission-mode="default">
  <div class="Wr"></div>
  <div class="fo">
    <div
      contenteditable="plaintext-only"
      class="d"
      role="textbox"
      aria-label="Message input"
      aria-multiline="true"
      data-placeholder="Ask Claude to edit…"
    ></div>
  </div>
  <div class="ri">
    <!-- Footer buttons -->
  </div>
</form>
```

#### 可复用的 ContentEditable 输入框

```typescript
// 从 Claude Code 提取的 ContentEditable 模式
import React, { useRef, useEffect } from 'react';

interface ContentEditableInputProps {
  value: string;
  onChange: (value: string) => void;
  onKeyDown?: (e: React.KeyboardEvent) => void;
  placeholder?: string;
  className?: string;
  autoFocus?: boolean;
}

export const ContentEditableInput: React.FC<ContentEditableInputProps> = ({
  value,
  onChange,
  onKeyDown,
  placeholder,
  className,
  autoFocus
}) => {
  const inputRef = useRef<HTMLDivElement>(null);

  // 同步外部 value 到 contentEditable
  useEffect(() => {
    if (inputRef.current && inputRef.current.textContent !== value) {
      inputRef.current.textContent = value;
    }
  }, [value]);

  // ��动聚焦
  useEffect(() => {
    if (autoFocus && inputRef.current) {
      inputRef.current.focus();
    }
  }, [autoFocus]);

  const handleInput = () => {
    if (inputRef.current) {
      const newValue = inputRef.current.textContent || '';
      onChange(newValue);
    }
  };

  const showPlaceholder = !value;

  return (
    <div className={`input-wrapper ${className || ''}`}>
      {showPlaceholder && (
        <div className="input-placeholder">{placeholder}</div>
      )}
      <div
        ref={inputRef}
        className="input-editable"
        contentEditable="plaintext-only"
        role="textbox"
        aria-label={placeholder}
        aria-multiline="true"
        onInput={handleInput}
        onKeyDown={onKeyDown}
        spellCheck={false}
        suppressContentEditableWarning
      />
    </div>
  );
};
```

**关键特性**:

- ✅ `contentEditable="plaintext-only"` 防止富文本
- ✅ `suppressContentEditableWarning` 避免 React 警告
- ✅ `role="textbox"` 改善无障碍

---

### 3. 权限请求对话框逻辑

#### 从混淆代码推断的交互逻辑

```typescript
// 权限请求对话框的键盘导航
import React, { useState, useEffect, useRef, useCallback } from 'react';

interface PermissionOption {
  id: string;
  label: string;
  shortcutKey: string;
}

interface PermissionRequestProps {
  title: string;
  options: PermissionOption[];
  onSelect: (optionId: string, customMessage?: string) => void;
  onReject: (reason?: string) => void;
}

export const PermissionRequest: React.FC<PermissionRequestProps> = ({
  title,
  options,
  onSelect,
  onReject
}) => {
  const [focusedIndex, setFocusedIndex] = useState(0);
  const [rejectMessage, setRejectMessage] = useState('');
  const [isReady, setIsReady] = useState(false);

  const buttonRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const containerRef = useRef<HTMLDivElement>(null);

  // 延迟显示(防止立即响应按键)
  useEffect(() => {
    const timer = setTimeout(() => {
      setIsReady(true);
      // 聚焦第一个按钮
      if (buttonRefs.current[0]) {
        buttonRefs.current[0].focus();
      }
    }, 800);

    return () => clearTimeout(timer);
  }, []);

  // 键盘导航
  const handleKeyDown = useCallback((e: KeyboardEvent) => {
    if (!isReady) return;

    // 数字快捷键
    const numberShortcuts: Record<string, () => void> = {};
    options.forEach((option, index) => {
      numberShortcuts[(index + 1).toString()] = () => {
        onSelect(option.id);
      };
    });

    if (!e.metaKey && !e.ctrlKey && numberShortcuts[e.key]) {
      e.preventDefault();
      numberShortcuts[e.key]();
      return;
    }

    // 方向键导航
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        setFocusedIndex(prev =>
          Math.min(prev + 1, buttonRefs.current.length - 1)
        );
        break;
      case 'ArrowUp':
        e.preventDefault();
        setFocusedIndex(prev => Math.max(prev - 1, 0));
        break;
      case 'Escape':
        if (!e.metaKey && !e.ctrlKey) {
          e.preventDefault();
          onReject(rejectMessage || undefined);
        }
        break;
    }
  }, [isReady, options, onSelect, onReject, rejectMessage]);

  useEffect(() => {
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [handleKeyDown]);

  // 聚焦当前索引的按钮
  useEffect(() => {
    buttonRefs.current[focusedIndex]?.focus();
  }, [focusedIndex]);

  return (
    <div
      ref={containerRef}
      className="permission-request-container"
      data-focused-index={focusedIndex}
      tabIndex={0}
    >
      <div className="permission-request-background" />

      <div className="permission-request-content">
        <div className="permission-title">{title}</div>

        <div className="permission-options">
          {options.map((option, index) => (
            <button
              key={option.id}
              ref={el => buttonRefs.current[index] = el}
              className={`permission-option ${
                index === focusedIndex ? 'focused' : ''
              }`}
              onClick={() => onSelect(option.id)}
              disabled={!isReady}
            >
              <span className="shortcut-key">{option.shortcutKey}</span>
              {' '}
              {option.label}
            </button>
          ))}

          {/* 拒绝消息输入框 */}
          <div className="reject-message-input">
            <input
              type="text"
              placeholder="Tell Claude what to do instead"
              value={rejectMessage}
              onChange={e => setRejectMessage(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault();
                  onReject(rejectMessage);
                } else if (e.key === 'Escape') {
                  e.preventDefault();
                  onReject();
                }
              }}
            />
          </div>
        </div>
      </div>
    </div>
  );
};
```

**关键逻辑**:

- ✅ 800ms 延迟防止误触
- ✅ 数字键快捷方式 (1/2/3)
- ✅ 方向键导航
- ✅ `data-focused-index` 用于 CSS 高亮
- ✅ Escape 取消

---

### 4. Session 列表下拉菜单

#### 从 onClick 处理推断的交互

```typescript
// Session 切换逻辑
import React, { useState, useEffect, useRef } from 'react';

interface Session {
  id: string;
  title: string;
  lastUpdated: string;
  messageCount: number;
}

interface SessionSelectorProps {
  sessions: Session[];
  currentSessionId?: string;
  onSelectSession: (sessionId: string) => void;
  onNewSession: () => void;
  onClose: () => void;
}

export const SessionSelector: React.FC<SessionSelectorProps> = ({
  sessions,
  currentSessionId,
  onSelectSession,
  onNewSession,
  onClose
}) => {
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // 点击外部关闭
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        dropdownRef.current &&
        !dropdownRef.current.contains(event.target as Node)
      ) {
        onClose();
      }
    };

    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [onClose]);

  // 键盘导航
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [onClose]);

  const getTimeAgo = (timestamp: string): string => {
    const now = Date.now();
    const time = new Date(timestamp).getTime();
    const diff = now - time;

    const minutes = Math.floor(diff / 60000);
    const hours = Math.floor(diff / 3600000);
    const days = Math.floor(diff / 86400000);

    if (minutes < 1) return 'Just now';
    if (minutes < 60) return `${minutes}m ago`;
    if (hours < 24) return `${hours}h ago`;
    if (days < 7) return `${days}d ago`;

    return new Date(time).toLocaleDateString();
  };

  return (
    <div ref={dropdownRef} className="session-selector-dropdown">
      <div className="session-dropdown-header">
        <span>Recent Sessions</span>
        <button onClick={onNewSession} className="new-session-mini-button">
          ➕ New
        </button>
      </div>

      <div className="session-dropdown-list">
        {sessions.length === 0 ? (
          <div className="no-sessions">No sessions available</div>
        ) : (
          sessions.map(session => (
            <div
              key={session.id}
              className={`session-dropdown-item ${
                session.id === currentSessionId ? 'active' : ''
              } ${hoveredId === session.id ? 'hovered' : ''}`}
              onMouseEnter={() => setHoveredId(session.id)}
              onMouseLeave={() => setHoveredId(null)}
              onClick={() => {
                onSelectSession(session.id);
                onClose();
              }}
            >
              <div className="session-item-title">{session.title}</div>
              <div className="session-item-meta">
                <span className="session-time">
                  {getTimeAgo(session.lastUpdated)}
                </span>
                {session.messageCount > 0 && (
                  <span className="session-count">
                    {session.messageCount} messages
                  </span>
                )}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
};
```

**提取的设计模式**:

- ✅ 悬停高亮 + 当前项高亮
- ✅ 点击外部关闭
- ✅ 时间显示为相对时间 (5m ago)
- ✅ New 按钮在 header 内

---

## 二、无法提取但可推断的逻辑

### 1. Session 切换的数据流

虽然具体实现被混淆,但从 HTML 和 CSS 可以推断:

```typescript
// 推断的数据流
interface ChatContext {
  currentSessionId: string | null;
  sessions: Session[];
  loadSession: (sessionId: string) => Promise<void>;
  createSession: () => Promise<Session>;
}

// 切换 Session 的流程
async function handleSwitchSession(sessionId: string) {
  // 1. 发送消息给扩展
  vscode.postMessage({
    type: 'switchSession',
    sessionId,
  });

  // 2. 等待扩展响应
  // (扩展会发送 'sessionSwitched' 消息回来)

  // 3. 更新 UI
  // setCurrentSessionId(sessionId);
  // setMessages(sessionMessages);
}

// 新建 Session 的流程
async function handleNewSession() {
  // 1. 发送消息
  vscode.postMessage({
    type: 'newSession',
  });

  // 2. 清空当前 UI
  // setMessages([]);
  // setCurrentStreamContent('');

  // 3. 扩展会返回新 sessionId
}
```

### 2. Message State 管理

```typescript
// 从 React 组件模式推断的状态结构
interface MessageState {
  messages: ChatMessage[];
  currentStreamContent: string;
  isStreaming: boolean;
  permissionRequest: PermissionRequest | null;
}

// 处理流式消息
function handleStreamChunk(chunk: string) {
  setCurrentStreamContent((prev) => prev + chunk);
}

function handleStreamEnd() {
  // 将流式内容添加到消息列表
  setMessages((prev) => [
    ...prev,
    {
      role: 'assistant',
      content: currentStreamContent,
      timestamp: Date.now(),
    },
  ]);

  // 清空流式缓冲
  setCurrentStreamContent('');
  setIsStreaming(false);
}
```

---

## 三、可直接复制的代码片段

### 1. Fade-in 动画 CSS

```css
/* 从 Claude Code 提取的动画 */
@keyframes fadeIn {
  from {
    opacity: 0;
    transform: translateY(10px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.session-selector-dropdown {
  animation: fadeIn 0.2s ease-out;
}

.message {
  animation: fadeIn 0.3s ease-out;
}
```

### 2. 脉冲动画 (加载指示器)

```css
@keyframes pulse {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.3;
  }
}

.tool-call.status-in-progress:before {
  animation: pulse 1s linear infinite;
}
```

### 3. 自动滚动到底部

```typescript
// 从 React 模式提取
const messagesEndRef = useRef<HTMLDivElement>(null);

useEffect(() => {
  messagesEndRef.current?.scrollIntoView({
    behavior: 'smooth'
  });
}, [messages, currentStreamContent]);

// JSX
<div className="messages-container">
  {messages.map(msg => <Message key={msg.id} {...msg} />)}
  <div ref={messagesEndRef} />
</div>
```

### 4. VSCode API 类型安全封装

```typescript
// 从 useVSCode hook 推断
interface VSCodeAPI {
  postMessage(message: unknown): void;
  getState(): unknown;
  setState(state: unknown): void;
}

declare global {
  interface Window {
    acquireVsCodeApi(): VSCodeAPI;
  }
}

export function useVSCode() {
  const vscode = useRef<VSCodeAPI>();

  if (!vscode.current) {
    if (typeof acquireVsCodeApi !== 'undefined') {
      vscode.current = acquireVsCodeApi();
    } else {
      // Mock for development
      vscode.current = {
        postMessage: (msg) => console.log('Mock postMessage:', msg),
        getState: () => ({}),
        setState: () => {},
      };
    }
  }

  return vscode.current;
}
```

---

## 四、关键发现总结

### ✅ 可以直接复用的

| 内容                 | 来源           | 可用性 |
| -------------------- | -------------- | ------ |
| CSS 样式             | index.css      | 100%   |
| HTML 结构            | body innerHTML | 100%   |
| 键盘导航逻辑         | 事件处理推断   | 90%    |
| 动画效果             | CSS keyframes  | 100%   |
| ContentEditable 模式 | HTML 属性      | 100%   |

### ⚠️ 需要自行实现的

| 内容         | 原因         | 实现难度      |
| ------------ | ------------ | ------------- |
| WebView 通信 | 业务逻辑混淆 | 低 (参考文档) |
| Session 管理 | 状态管理混淆 | 中 (推断可行) |
| 流式响应处理 | 协议层混淆   | 低 (已有实现) |

### ❌ 完全无法提取的

- React 组件的具体 props
- 内部状态管理逻辑
- API 调用细节

---

## 五、推荐实现策略

### 方案 A: 混合复用 (推荐 ⭐⭐⭐⭐⭐)

```
1. 完全复制 CSS ✅
2. 参考 HTML 结构重写 React 组件 ✅
3. 自实现业务逻辑 ✅
```

**优点**:

- UI 100% 对标
- 代码可控
- 无版权风险

**实施步骤**:

1. 复制 CSS 到 `App.css`
2. 根据 HTML 创建 `ChatHeader.tsx`
3. 实现 `SessionSelector.tsx`
4. 实现 `PermissionRequest.tsx`
5. 集成到现有 `App.tsx`

### 方案 B: 关键组件提取

仅提取最核心的 3 个组件:

- ChatHeader
- SessionSelector
- ContentEditableInput

**时间估算**: 1-2 天

---

## 六、实战示例

### 完整的 ChatHeader 实现

```typescript
// src/webview/components/ChatHeader.tsx
import React from 'react';
import './ChatHeader.css';

interface ChatHeaderProps {
  currentSessionTitle: string;
  onSessionsClick: () => void;
  onNewChatClick: () => void;
}

export const ChatHeader: React.FC<ChatHeaderProps> = ({
  currentSessionTitle,
  onSessionsClick,
  onNewChatClick
}) => {
  return (
    <div className="chat-header">
      <button
        className="session-dropdown-button"
        title="Past conversations"
        onClick={onSessionsClick}
      >
        <span className="session-dropdown-content">
          <span className="session-title">
            {currentSessionTitle || 'Past Conversations'}
          </span>
          <svg
            className="dropdown-icon"
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 20 20"
            fill="currentColor"
          >
            <path
              fillRule="evenodd"
              d="M5.22 8.22a.75.75 0 0 1 1.06 0L10 11.94l3.72-3.72a.75.75 0 1 1 1.06 1.06l-4.25 4.25a.75.75 0 0 1-1.06 0L5.22 9.28a.75.75 0 0 1 0-1.06Z"
              clipRule="evenodd"
            />
          </svg>
        </span>
      </button>

      <div className="header-spacer"></div>

      <button
        className="new-session-button"
        title="New Session"
        onClick={onNewChatClick}
      >
        <svg
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 20 20"
          fill="currentColor"
        >
          <path d="M10.75 4.75a.75.75 0 0 0-1.5 0v4.5h-4.5a.75.75 0 0 0 0 1.5h4.5v4.5a.75.75 0 0 0 1.5 0v-4.5h4.5a.75.75 0 0 0 0-1.5h-4.5v-4.5Z" />
        </svg>
      </button>
    </div>
  );
};
```

```css
/* src/webview/components/ChatHeader.css */
.chat-header {
  display: flex;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
  padding: 6px 10px;
  gap: 4px;
  background-color: var(--vscode-sideBar-background);
  justify-content: flex-start;
  user-select: none;
}

.session-dropdown-button {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 2px 8px;
  background: transparent;
  border: none;
  border-radius: 4px;
  cursor: pointer;
  outline: none;
  min-width: 0;
  max-width: 300px;
  overflow: hidden;
  font-size: var(--vscode-chat-font-size, 13px);
  font-family: var(--vscode-chat-font-family);
}

.session-dropdown-button:focus,
.session-dropdown-button:hover {
  background: var(--vscode-toolbar-hoverBackground);
}

.session-dropdown-content {
  display: flex;
  align-items: center;
  gap: 4px;
  max-width: 300px;
  overflow: hidden;
}

.session-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 500;
}

.dropdown-icon {
  flex-shrink: 0;
  width: 16px;
  height: 16px;
  min-width: 16px;
}

.header-spacer {
  flex: 1;
}

.new-session-button {
  flex: 0 0 auto;
  padding: 0;
  background: transparent;
  border: 1px solid transparent;
  border-radius: 4px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  outline: none;
  width: 24px;
  height: 24px;
}

.new-session-button:focus,
.new-session-button:hover {
  background: var(--vscode-toolbar-hoverBackground);
}

.new-session-button svg {
  width: 16px;
  height: 16px;
}
```

---

## 七、总结

### 核心收获

1. **CSS 完全可用** - 直接复制无风险
2. **HTML 结构清晰** - 可准确还原 React 组件
3. **交互逻辑可推断** - 通过事件和状态推断
4. **业务逻辑需自写** - 但有明确的接口定义

### 最终建议

✅ **立即可做**:

- 复制 CSS 样式表
- 创建 ChatHeader 组件
- 实现 SessionSelector 下拉

⏸️ **后续优化**:

- Permission Request 对话框
- ContentEditable 输入优化
- 键盘导航增强

---

**文档版本**: v2.0
**最后更新**: 2025-11-18
**状态**: 已验证可行
