package com.alibaba.qwen.code.cli.session.event;

import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;
import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKUserMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlPermissionRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlResponse;
import com.alibaba.qwen.code.cli.session.Session;

public interface SessionEventConsumers {
    void onSystemMessage(Session session, SDKSystemMessage systemMessage);

    void onResultMessage(Session session, SDKResultMessage resultMessage);

    void onAssistantMessage(Session session, SDKAssistantMessage assistantMessage);

    void onUserMessage(Session session, SDKUserMessage userMessage);

    void onOtherMessage(Session session, String message);

    void onControlResponse(Session session, CLIControlResponse<?> cliControlResponse);

    CLIControlResponse<?> onControlRequest(Session session, CLIControlRequest<?> cliControlRequest);

    Behavior onPermissionRequest(Session session, CLIControlRequest<CLIControlPermissionRequest> permissionRequest);
}
