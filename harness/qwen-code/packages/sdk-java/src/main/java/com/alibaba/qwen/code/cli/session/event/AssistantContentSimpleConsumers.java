package com.alibaba.qwen.code.cli.session.event;

import com.alibaba.qwen.code.cli.protocol.data.AssistantUsage;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.TextAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ThingkingAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ToolResultAssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent.ToolUseAssistantContent;
import com.alibaba.qwen.code.cli.session.Session;

/**
 * Simple implementation of AssistantContentConsumers that provides empty implementations for all methods.
 *
 * @author skyfire
 * @version $Id: 0.0.1
 */
public class AssistantContentSimpleConsumers implements AssistantContentConsumers {
    /** {@inheritDoc} */
    @Override
    public void onText(Session session, TextAssistantContent textAssistantContent) {
    }

    /** {@inheritDoc} */
    @Override
    public void onThinking(Session session, ThingkingAssistantContent thingkingAssistantContent) {
    }

    /** {@inheritDoc} */
    @Override
    public void onToolUse(Session session, ToolUseAssistantContent toolUseAssistantContent) {
    }

    /** {@inheritDoc} */
    @Override
    public void onToolResult(Session session, ToolResultAssistantContent toolResultAssistantContent) {
    }

    /** {@inheritDoc} */
    @Override
    public void onOtherContent(Session session, AssistantContent<?> other) {
    }

    /** {@inheritDoc} */
    @Override
    public void onUsage(Session session, AssistantUsage AssistantUsage) {
    }
}
