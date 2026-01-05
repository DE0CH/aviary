package com.alibaba.qwen.code.cli.session.event.consumers;

import com.alibaba.qwen.code.cli.protocol.data.AssistantUsage;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.TextAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ThingkingAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ToolResultAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ToolUseAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Allow;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior.Operation;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Deny;
import com.alibaba.qwen.code.cli.protocol.message.control.payload.CLIControlPermissionRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.payload.ControlRequestPayload;
import com.alibaba.qwen.code.cli.protocol.message.control.payload.ControlResponsePayload;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.utils.Timeout;

/**
 * Simple implementation of AssistantContentConsumers that provides empty implementations for all methods.
 *
 * @author skyfire
 * @version $Id: 0.0.1
 */
public class AssistantContentSimpleConsumers implements AssistantContentConsumers {
    /**
     * {@inheritDoc}
     */
    @Override
    public void onText(Session session, TextAssistantContent textAssistantContent) {
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public void onThinking(Session session, ThingkingAssistantContent thingkingAssistantContent) {
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public void onToolUse(Session session, ToolUseAssistantContent toolUseAssistantContent) {
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public void onToolResult(Session session, ToolResultAssistantContent toolResultAssistantContent) {
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public void onOtherContent(Session session, AssistantContent<?> other) {
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public Behavior onPermissionRequest(Session session, CLIControlPermissionRequest permissionRequest) {
        if (Operation.deny.equals(this.defaultPermissionOperation)) {
            return new Deny().setMessage("Permission denied.");
        } else {
            return new Allow().setUpdatedInput(permissionRequest.getInput());
        }
    }

    @Override
    public ControlResponsePayload onOtherControlRequest(Session session, ControlRequestPayload requestPayload) {
        throw new RuntimeException("need override onOtherControlRequest");
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public void onUsage(Session session, AssistantUsage AssistantUsage) {
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public Timeout onPermissionRequestTimeout(Session session, CLIControlPermissionRequest permissionRequest) {
        return defaultEventTimeout;
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public Timeout onOtherControlRequestTimeout(Session session, ControlRequestPayload requestPayload) {
        return defaultEventTimeout;
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public Timeout onTextTimeout(Session session, TextAssistantContent textAssistantContent) {
        return defaultEventTimeout;
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public Timeout onThinkingTimeout(Session session, ThingkingAssistantContent thingkingAssistantContent) {
        return defaultEventTimeout;
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public Timeout onToolUseTimeout(Session session, ToolUseAssistantContent toolUseAssistantContent) {
        return defaultEventTimeout;
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public Timeout onToolResultTimeout(Session session, ToolResultAssistantContent toolResultAssistantContent) {
        return defaultEventTimeout;
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public Timeout onOtherContentTimeout(Session session, AssistantContent<?> other) {
        return defaultEventTimeout;
    }

    /**
     * {@inheritDoc}
     */
    @Override
    public AssistantContentSimpleConsumers setDefaultPermissionOperation(Operation defaultPermissionOperation) {
        this.defaultPermissionOperation = defaultPermissionOperation;
        return this;
    }

    /**
     * Constructor.
     *
     * @param defaultPermissionOperation The default permission operation.
     * @param defaultEventTimeout The default event timeout.
     */
    public AssistantContentSimpleConsumers(Operation defaultPermissionOperation, Timeout defaultEventTimeout) {
        this.defaultPermissionOperation = defaultPermissionOperation;
        this.defaultEventTimeout = defaultEventTimeout;
    }

    /**
     * Constructor.
     */
    public AssistantContentSimpleConsumers() {
    }

    /**
     * The default permission operation.
     */
    private Operation defaultPermissionOperation = Operation.deny;

    /**
     * The default event timeout.
     */
    protected Timeout defaultEventTimeout = Timeout.TIMEOUT_60_SECONDS;
}
