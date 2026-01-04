package com.alibaba.qwen.code.cli.session.event;

import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;
import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKUserMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKPartialAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlPermissionRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlResponse;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.utils.Timeout;

/**
 * Interface for handling different types of events during a session.
 */
public interface SessionEventConsumers {
    /**
     * Handles system messages.
     *
     * @param session The session
     * @param systemMessage The system message
     */
    void onSystemMessage(Session session, SDKSystemMessage systemMessage);

    /**
     * Handles result messages.
     *
     * @param session The session
     * @param resultMessage The result message
     */
    void onResultMessage(Session session, SDKResultMessage resultMessage);

    /**
     * Handles assistant messages.
     *
     * @param session The session
     * @param assistantMessage The assistant message
     */
    void onAssistantMessage(Session session, SDKAssistantMessage assistantMessage);

    /**
     * Handles partial assistant messages.
     *
     * @param session The session
     * @param partialAssistantMessage The partial assistant message
     */
    void onPartialAssistantMessage(Session session, SDKPartialAssistantMessage partialAssistantMessage);

    /**
     * Handles user messages.
     *
     * @param session The session
     * @param userMessage The user message
     */
    void onUserMessage(Session session, SDKUserMessage userMessage);

    /**
     * Handles other types of messages.
     *
     * @param session The session
     * @param message The message
     */
    void onOtherMessage(Session session, String message);

    /**
     * Handles control responses.
     *
     * @param session The session
     * @param cliControlResponse The control response
     */
    void onControlResponse(Session session, CLIControlResponse<?> cliControlResponse);

    /**
     * Handles control requests.
     *
     * @param session The session
     * @param cliControlRequest The control request
     * @return The control response
     */
    CLIControlResponse<?> onControlRequest(Session session, CLIControlRequest<?> cliControlRequest);

    /**
     * Handles permission requests.
     *
     * @param session The session
     * @param permissionRequest The permission request
     * @return The behavior for the permission request
     */
    Behavior onPermissionRequest(Session session, CLIControlRequest<CLIControlPermissionRequest> permissionRequest);

    /**
     * Gets timeout for system message handling.
     *
     * @param session The session
     * @return The timeout for system message handling
     */
    Timeout onSystemMessageTimeout(Session session);

    /**
     * Gets timeout for result message handling.
     *
     * @param session The session
     * @return The timeout for result message handling
     */
    Timeout onResultMessageTimeout(Session session);

    /**
     * Gets timeout for assistant message handling.
     *
     * @param session The session
     * @return The timeout for assistant message handling
     */
    Timeout onAssistantMessageTimeout(Session session);

    /**
     * Gets timeout for partial assistant message handling.
     *
     * @param session The session
     * @return The timeout for partial assistant message handling
     */
    Timeout onPartialAssistantMessageTimeout(Session session);

    /**
     * Gets timeout for user message handling.
     *
     * @param session The session
     * @return The timeout for user message handling
     */
    Timeout onUserMessageTimeout(Session session);

    /**
     * Gets timeout for other message handling.
     *
     * @param session The session
     * @return The timeout for other message handling
     */
    Timeout onOtherMessageTimeout(Session session);

    /**
     * Gets timeout for control response handling.
     *
     * @param session The session
     * @return The timeout for control response handling
     */
    Timeout onControlResponseTimeout(Session session);

    /**
     * Gets timeout for control request handling.
     *
     * @param session The session
     * @return The timeout for control request handling
     */
    Timeout onControlRequestTimeout(Session session);

    /**
     * Gets timeout for permission request handling.
     *
     * @param session The session
     * @return The timeout for permission request handling
     */
    Timeout onPermissionRequestTimeout(Session session);
}
