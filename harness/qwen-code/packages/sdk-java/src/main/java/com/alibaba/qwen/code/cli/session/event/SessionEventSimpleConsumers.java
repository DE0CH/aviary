package com.alibaba.qwen.code.cli.session.event;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Optional;
import java.util.stream.Collectors;

import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Allow;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior.Operation;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Deny;
import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKUserMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKPartialAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.event.ContentBlockDeltaEvent;
import com.alibaba.qwen.code.cli.protocol.message.assistant.event.StreamEvent;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlPermissionRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlResponse;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.utils.Timeout;

public class SessionEventSimpleConsumers implements SessionEventConsumers {
    @Override
    public void onSystemMessage(Session session, SDKSystemMessage systemMessage) {
    }

    @Override
    public void onResultMessage(Session session, SDKResultMessage resultMessage) {
    }

    @Override
    public void onAssistantMessage(Session session, SDKAssistantMessage assistantMessage) {
        onAssistantMessageIncludePartial(session, Optional.ofNullable(assistantMessage.getMessage().getContent())
                .map(cbs -> cbs.stream().map(cb -> (AssistantContent) cb).collect(Collectors.toList()))
                .orElse(new ArrayList<>()), AssistantMessageOutputType.entire);
    }

    @Override
    public void onPartialAssistantMessage(Session session, SDKPartialAssistantMessage partialAssistantMessage) {
        StreamEvent event = partialAssistantMessage.getEvent();
        if (!(event instanceof ContentBlockDeltaEvent)) {
            return;
        }
        onAssistantMessageIncludePartial(session, Collections.singletonList(((ContentBlockDeltaEvent) event).getDelta()), AssistantMessageOutputType.partial);
    }

    public void onAssistantMessageIncludePartial(Session session, List<AssistantContent> assistantContents,
            AssistantMessageOutputType assistantMessageOutputType) {
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
        if (Operation.deny.equals(this.defaultPermissionOperation)) {
            return new Deny().setMessage("Permission denied.");
        } else {
            return new Allow().setUpdatedInput(permissionRequest.getRequest().getInput());
        }
    }

    @Override
    public Timeout onSystemMessageTimeout(Session session) {
        return defaultEventTimeout;
    }

    @Override
    public Timeout onResultMessageTimeout(Session session) {
        return defaultEventTimeout;
    }

    @Override
    public Timeout onAssistantMessageTimeout(Session session) {
        return defaultEventTimeout;
    }

    @Override
    public Timeout onPartialAssistantMessageTimeout(Session session) {
        return defaultEventTimeout;
    }

    @Override
    public Timeout onUserMessageTimeout(Session session) {
        return defaultEventTimeout;
    }

    @Override
    public Timeout onOtherMessageTimeout(Session session) {
        return defaultEventTimeout;
    }

    @Override
    public Timeout onControlResponseTimeout(Session session) {
        return defaultEventTimeout;
    }

    @Override
    public Timeout onControlRequestTimeout(Session session) {
        return defaultEventTimeout;
    }

    @Override
    public Timeout onPermissionRequestTimeout(Session session) {
        return defaultEventTimeout;
    }

    public Timeout getDefaultEventTimeout() {
        return defaultEventTimeout;
    }

    public SessionEventSimpleConsumers setDefaultEventTimeout(Timeout defaultEventTimeout) {
        this.defaultEventTimeout = defaultEventTimeout;
        return this;
    }

    public Operation getDefaultPermissionOperation() {
        return defaultPermissionOperation;
    }

    public SessionEventSimpleConsumers setDefaultPermissionOperation(Operation defaultPermissionOperation) {
        this.defaultPermissionOperation = defaultPermissionOperation;
        return this;
    }

    public SessionEventSimpleConsumers() {
    }

    public SessionEventSimpleConsumers(Operation defaultPermissionOperation, Timeout defaultEventTimeout) {
        this.defaultPermissionOperation = defaultPermissionOperation;
        this.defaultEventTimeout = defaultEventTimeout;
    }

    private Operation defaultPermissionOperation = Operation.deny;
    protected Timeout defaultEventTimeout = Timeout.TIMEOUT_60_SECONDS;

    public enum AssistantMessageOutputType {
        entire,
        partial
    }
}
