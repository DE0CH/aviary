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

public class SessionEventSimpleConsumers implements SessionEventConsumers {
    @Override
    public void onSystemMessage(Session session, SDKSystemMessage systemMessage) {
    }

    @Override
    public void onResultMessage(Session session, SDKResultMessage resultMessage) {
    }

    @Override
    public void onAssistantMessage(Session session, SDKAssistantMessage assistantMessage) {
    }

    @Override
    public void onUserMessage(Session session, SDKUserMessage userMessage) {
    }

    @Override
    public void onOtherMessage(Session session, String message) {
    }

    @Override
    public void onControlResponse(Session session, CLIControlResponse<?> cliControlResponse) {
    }

    @Override
    public CLIControlResponse<?> onControlRequest(Session session, CLIControlRequest<?> cliControlRequest) {
        return new CLIControlResponse<>();
    }

    @Override
    public Behavior onPermissionRequest(Session session, CLIControlRequest<CLIControlPermissionRequest> permissionRequest) {
        return Behavior.defaultBehavior();
    }
}
