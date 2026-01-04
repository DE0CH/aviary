package com.alibaba.qwen.code.cli.session.event;

import com.alibaba.qwen.code.cli.protocol.data.AssistantUsage;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.TextAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ThingkingAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ToolResultAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ToolUseAssistantContent;
import com.alibaba.qwen.code.cli.session.Session;

/**
 * Interface for handling different types of assistant content during a session.
 */
public interface AssistantContentConsumers {
    /**
     * Handles text content from the assistant.
     *
     * @param session The session
     * @param textAssistantContent The text content from the assistant
     */
    void onText(Session session, TextAssistantContent textAssistantContent);

    /**
     * Handles thinking content from the assistant.
     *
     * @param session The session
     * @param thingkingAssistantContent The thinking content from the assistant
     */
    void onThinking(Session session, ThingkingAssistantContent thingkingAssistantContent);

    /**
     * Handles tool use content from the assistant.
     *
     * @param session The session
     * @param toolUseAssistantContent The tool use content from the assistant
     */
    void onToolUse(Session session, ToolUseAssistantContent toolUseAssistantContent);

    /**
     * Handles tool result content from the assistant.
     *
     * @param session The session
     * @param toolResultAssistantContent The tool result content from the assistant
     */
    void onToolResult(Session session, ToolResultAssistantContent toolResultAssistantContent);

    /**
     * Handles other types of assistant content.
     *
     * @param session The session
     * @param other The other content from the assistant
     */
    void onOtherContent(Session session, AssistantContent<?> other);

    /**
     * Handles usage information from the assistant.
     *
     * @param session The session
     * @param AssistantUsage The usage information from the assistant
     */
    void onUsage(Session session, AssistantUsage AssistantUsage);
}
