package com.alibaba.qwen.code.cli.session.event;

import java.util.List;

import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.TextAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ThingkingAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ToolResultAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ToolUseAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantUsage;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Allow;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior.Operation;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Deny;
import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKUserMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKPartialAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.block.ContentBlock;
import com.alibaba.qwen.code.cli.protocol.message.assistant.event.ContentBlockDeltaEvent;
import com.alibaba.qwen.code.cli.protocol.message.assistant.event.StreamEvent;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlPermissionRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlResponse;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.utils.Timeout;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * Simple implementation of SessionEventConsumers that provides basic implementations for all methods.
 */
public class SessionEventSimpleConsumers implements SessionEventConsumers {
    @Override
    public void onSystemMessage(Session session, SDKSystemMessage systemMessage) {
    }

    @Override
    public void onResultMessage(Session session, SDKResultMessage resultMessage) {
    }

    @Override
    public void onAssistantMessage(Session session, SDKAssistantMessage assistantMessage) {
        List<ContentBlock<?>> contentBlocks = assistantMessage.getMessage().getContent();
        if (assistantContentConsumers == null || contentBlocks == null || contentBlocks.isEmpty()) {
            return;
        }
        assistantContentConsumers.onUsage(session, new AssistantUsage(assistantMessage.getMessage().getId(), assistantMessage.getMessage().getUsage()));

        if (!session.isStreaming()) {
            contentBlocks.forEach(contentBlock -> consumeAssistantContent(session, contentBlock));
        }
    }

    @Override
    public void onPartialAssistantMessage(Session session, SDKPartialAssistantMessage partialAssistantMessage) {
        StreamEvent event = partialAssistantMessage.getEvent();
        if (!(event instanceof ContentBlockDeltaEvent)) {
            log.debug("received partialAssistantMessage and is not instance of ContentBlockDeltaEvent, will ignore process. the message is {}",
                    partialAssistantMessage);
            return;
        }
        ContentBlockDeltaEvent contentBlockDeltaEvent = (ContentBlockDeltaEvent) event;
        contentBlockDeltaEvent.getDelta().setMessageId(partialAssistantMessage.getMessageId());
        consumeAssistantContent(session, contentBlockDeltaEvent.getDelta());
    }

    protected void consumeAssistantContent(Session session, AssistantContent<?> assistantContent) {
        if (assistantContent instanceof TextAssistantContent) {
            assistantContentConsumers.onText(session, (TextAssistantContent) assistantContent);
        } else if (assistantContent instanceof ThingkingAssistantContent) {
            assistantContentConsumers.onThinking(session, (ThingkingAssistantContent) assistantContent);
        } else if (assistantContent instanceof ToolUseAssistantContent) {
            assistantContentConsumers.onToolUse(session, (ToolUseAssistantContent) assistantContent);
        } else if (assistantContent instanceof ToolResultAssistantContent) {
            assistantContentConsumers.onToolResult(session, (ToolResultAssistantContent) assistantContent);
        } else {
            assistantContentConsumers.onOtherContent(session, assistantContent);
        }
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

    /**
     * Gets the default event timeout.
     *
     * @return The default event timeout
     */
    protected Timeout getDefaultEventTimeout() {
        return defaultEventTimeout;
    }

    /**
     * Sets the default event timeout.
     *
     * @param defaultEventTimeout The default event timeout
     * @return This instance for method chaining
     */
    public SessionEventSimpleConsumers setDefaultEventTimeout(Timeout defaultEventTimeout) {
        this.defaultEventTimeout = defaultEventTimeout;
        return this;
    }

    /**
     * Gets the default permission operation.
     *
     * @return The default permission operation
     */
    protected Operation getDefaultPermissionOperation() {
        return defaultPermissionOperation;
    }

    /**
     * Sets the default permission operation.
     *
     * @param defaultPermissionOperation The default permission operation
     * @return This instance for method chaining
     */
    public SessionEventSimpleConsumers setDefaultPermissionOperation(Operation defaultPermissionOperation) {
        this.defaultPermissionOperation = defaultPermissionOperation;
        return this;
    }

    /**
     * Creates a new SessionEventSimpleConsumers instance with default values.
     */
    public SessionEventSimpleConsumers() {
    }

    /**
     * Creates a new SessionEventSimpleConsumers instance with the specified parameters.
     *
     * @param defaultPermissionOperation The default permission operation
     * @param defaultEventTimeout The default event timeout
     * @param assistantContentConsumers The assistant content consumers
     */
    public SessionEventSimpleConsumers(Operation defaultPermissionOperation, Timeout defaultEventTimeout,
            AssistantContentConsumers assistantContentConsumers) {
        this.defaultPermissionOperation = defaultPermissionOperation;
        this.defaultEventTimeout = defaultEventTimeout;
        this.assistantContentConsumers = assistantContentConsumers;
    }

    /**
     * The default permission operation.
     */
    private Operation defaultPermissionOperation = Operation.deny;
    /**
     * The default event timeout.
     */
    protected Timeout defaultEventTimeout = Timeout.TIMEOUT_60_SECONDS;
    /**
     * The assistant content consumers.
     */
    protected AssistantContentConsumers assistantContentConsumers;
    private static final Logger log = LoggerFactory.getLogger(SessionEventSimpleConsumers.class);

    /**
     * Sets the assistant content consumers.
     *
     * @param assistantContentConsumers The assistant content consumers
     * @return This instance for method chaining
     */
    public SessionEventSimpleConsumers setBlockConsumer(AssistantContentConsumers assistantContentConsumers) {
        this.assistantContentConsumers = assistantContentConsumers;
        return this;
    }
}
