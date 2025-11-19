/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { JSONRPC_VERSION } from '../shared/acpTypes.js';
import type {
  AcpBackend,
  AcpMessage,
  AcpPermissionRequest,
  AcpResponse,
  AcpSessionUpdate,
} from '../shared/acpTypes.js';
import type { ChildProcess, SpawnOptions } from 'child_process';
import { spawn } from 'child_process';
import type {
  PendingRequest,
  AcpConnectionCallbacks,
} from './connectionTypes.js';
import { AcpMessageHandler } from './acpMessageHandler.js';
import { AcpSessionManager } from './acpSessionManager.js';

/**
 * ACP Connection Handler for VSCode Extension
 *
 * This class implements the client side of the ACP (Agent Communication Protocol).
 *
 * Implementation Status:
 *
 * Client Methods (Methods this class implements, called by CLI):
 * ✅ session/update - Handle session updates via onSessionUpdate callback
 * ✅ session/request_permission - Request user permission for tool execution
 * ✅ fs/read_text_file - Read file from workspace
 * ✅ fs/write_text_file - Write file to workspace
 *
 * Agent Methods (Methods CLI implements, called by this class):
 * ✅ initialize - Initialize ACP protocol connection
 * ✅ authenticate - Authenticate with selected auth method
 * ✅ session/new - Create new chat session
 * ✅ session/prompt - Send user message to agent
 * ✅ session/cancel - Cancel current generation
 * ✅ session/load - Load previous session
 *
 * Custom Methods (Not in standard ACP):
 * ⚠️  session/list - List available sessions (custom extension)
 * ⚠️  session/switch - Switch to different session (custom extension)
 */
export class AcpConnection {
  private child: ChildProcess | null = null;
  private pendingRequests = new Map<number, PendingRequest<unknown>>();
  private nextRequestId = { value: 0 };
  private backend: AcpBackend | null = null;

  // 模块实例
  private messageHandler: AcpMessageHandler;
  private sessionManager: AcpSessionManager;

  // 回调函数
  onSessionUpdate: (data: AcpSessionUpdate) => void = () => {};
  onPermissionRequest: (data: AcpPermissionRequest) => Promise<{
    optionId: string;
  }> = () => Promise.resolve({ optionId: 'allow' });
  onEndTurn: () => void = () => {};

  constructor() {
    this.messageHandler = new AcpMessageHandler();
    this.sessionManager = new AcpSessionManager();
  }

  /**
   * 连接到ACP后端
   *
   * @param backend - 后端类型
   * @param cliPath - CLI路径
   * @param workingDir - 工作目录
   * @param extraArgs - 额外的命令行参数
   */
  async connect(
    backend: AcpBackend,
    cliPath: string,
    workingDir: string = process.cwd(),
    extraArgs: string[] = [],
  ): Promise<void> {
    if (this.child) {
      this.disconnect();
    }

    this.backend = backend;

    const isWindows = process.platform === 'win32';
    const env = { ...process.env };

    // 如果在extraArgs中配置了代理，也将其设置为环境变量
    // 这确保token刷新请求也使用代理
    const proxyArg = extraArgs.find(
      (arg, i) => arg === '--proxy' && i + 1 < extraArgs.length,
    );
    if (proxyArg) {
      const proxyIndex = extraArgs.indexOf('--proxy');
      const proxyUrl = extraArgs[proxyIndex + 1];
      console.log('[ACP] Setting proxy environment variables:', proxyUrl);

      env.HTTP_PROXY = proxyUrl;
      env.HTTPS_PROXY = proxyUrl;
      env.http_proxy = proxyUrl;
      env.https_proxy = proxyUrl;
    }

    let spawnCommand: string;
    let spawnArgs: string[];

    if (cliPath.startsWith('npx ')) {
      const parts = cliPath.split(' ');
      spawnCommand = isWindows ? 'npx.cmd' : 'npx';
      spawnArgs = [...parts.slice(1), '--experimental-acp', ...extraArgs];
    } else {
      spawnCommand = cliPath;
      spawnArgs = ['--experimental-acp', ...extraArgs];
    }

    console.log('[ACP] Spawning command:', spawnCommand, spawnArgs.join(' '));

    const options: SpawnOptions = {
      cwd: workingDir,
      stdio: ['pipe', 'pipe', 'pipe'],
      env,
      shell: isWindows,
    };

    this.child = spawn(spawnCommand, spawnArgs, options);
    await this.setupChildProcessHandlers(backend);
  }

  /**
   * 设置子进程处理器
   *
   * @param backend - 后端名称
   */
  private async setupChildProcessHandlers(backend: string): Promise<void> {
    let spawnError: Error | null = null;

    this.child!.stderr?.on('data', (data) => {
      const message = data.toString();
      if (
        message.toLowerCase().includes('error') &&
        !message.includes('Loaded cached')
      ) {
        console.error(`[ACP ${backend}]:`, message);
      } else {
        console.log(`[ACP ${backend}]:`, message);
      }
    });

    this.child!.on('error', (error) => {
      spawnError = error;
    });

    this.child!.on('exit', (code, signal) => {
      console.error(
        `[ACP ${backend}] Process exited with code: ${code}, signal: ${signal}`,
      );
    });

    // 等待进程启动
    await new Promise((resolve) => setTimeout(resolve, 1000));

    if (spawnError) {
      throw spawnError;
    }

    if (!this.child || this.child.killed) {
      throw new Error(`${backend} ACP process failed to start`);
    }

    // 处理来自ACP服务器的消息
    let buffer = '';
    this.child.stdout?.on('data', (data) => {
      buffer += data.toString();
      const lines = buffer.split('\n');
      buffer = lines.pop() || '';

      for (const line of lines) {
        if (line.trim()) {
          try {
            const message = JSON.parse(line) as AcpMessage;
            this.handleMessage(message);
          } catch (_error) {
            // 忽略非JSON行
          }
        }
      }
    });

    // 初始化协议
    await this.sessionManager.initialize(
      this.child,
      this.pendingRequests,
      this.nextRequestId,
    );
  }

  /**
   * 处理接收到的消息
   *
   * @param message - ACP消息
   */
  private handleMessage(message: AcpMessage): void {
    const callbacks: AcpConnectionCallbacks = {
      onSessionUpdate: this.onSessionUpdate,
      onPermissionRequest: this.onPermissionRequest,
      onEndTurn: this.onEndTurn,
    };

    // 处理消息
    if ('method' in message) {
      // 请求或通知
      this.messageHandler
        .handleIncomingRequest(message, callbacks)
        .then((result) => {
          if ('id' in message && typeof message.id === 'number') {
            this.messageHandler.sendResponseMessage(this.child, {
              jsonrpc: JSONRPC_VERSION,
              id: message.id,
              result,
            });
          }
        })
        .catch((error) => {
          if ('id' in message && typeof message.id === 'number') {
            this.messageHandler.sendResponseMessage(this.child, {
              jsonrpc: JSONRPC_VERSION,
              id: message.id,
              error: {
                code: -32603,
                message: error instanceof Error ? error.message : String(error),
              },
            });
          }
        });
    } else {
      // 响应
      this.messageHandler.handleMessage(
        message,
        this.pendingRequests,
        callbacks,
      );
    }
  }

  /**
   * 认证
   *
   * @param methodId - 认证方法ID
   * @returns 认证响应
   */
  async authenticate(methodId?: string): Promise<AcpResponse> {
    return this.sessionManager.authenticate(
      methodId,
      this.child,
      this.pendingRequests,
      this.nextRequestId,
    );
  }

  /**
   * 创建新会话
   *
   * @param cwd - 工作目录
   * @returns 新会话响应
   */
  async newSession(cwd: string = process.cwd()): Promise<AcpResponse> {
    return this.sessionManager.newSession(
      cwd,
      this.child,
      this.pendingRequests,
      this.nextRequestId,
    );
  }

  /**
   * 发送提示消息
   *
   * @param prompt - 提示内容
   * @returns 响应
   */
  async sendPrompt(prompt: string): Promise<AcpResponse> {
    return this.sessionManager.sendPrompt(
      prompt,
      this.child,
      this.pendingRequests,
      this.nextRequestId,
    );
  }

  /**
   * 加载已有会话
   *
   * @param sessionId - 会话ID
   * @returns 加载响应
   */
  async loadSession(sessionId: string): Promise<AcpResponse> {
    return this.sessionManager.loadSession(
      sessionId,
      this.child,
      this.pendingRequests,
      this.nextRequestId,
    );
  }

  /**
   * 获取会话列表
   *
   * @returns 会话列表响应
   */
  async listSessions(): Promise<AcpResponse> {
    return this.sessionManager.listSessions(
      this.child,
      this.pendingRequests,
      this.nextRequestId,
    );
  }

  /**
   * 切换到指定会话
   *
   * @param sessionId - 会话ID
   * @returns 切换响应
   */
  async switchSession(sessionId: string): Promise<AcpResponse> {
    return this.sessionManager.switchSession(sessionId, this.nextRequestId);
  }

  /**
   * 取消当前会话的提示生成
   */
  async cancelSession(): Promise<void> {
    await this.sessionManager.cancelSession(this.child);
  }

  /**
   * 断开连接
   */
  disconnect(): void {
    if (this.child) {
      this.child.kill();
      this.child = null;
    }

    this.pendingRequests.clear();
    this.sessionManager.reset();
    this.backend = null;
  }

  /**
   * 检查是否已连接
   */
  get isConnected(): boolean {
    return this.child !== null && !this.child.killed;
  }

  /**
   * 检查是否有活动会话
   */
  get hasActiveSession(): boolean {
    return this.sessionManager.getCurrentSessionId() !== null;
  }

  /**
   * 获取当前会话ID
   */
  get currentSessionId(): string | null {
    return this.sessionManager.getCurrentSessionId();
  }
}
