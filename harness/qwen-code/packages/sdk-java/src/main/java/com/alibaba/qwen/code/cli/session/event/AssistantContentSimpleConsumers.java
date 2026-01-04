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
 */
public class AssistantContentSimpleConsumers implements AssistantContentConsumers {
    @Override
    public void onText(Session session, TextAssistantContent textAssistantContent) {
    }

    @Override
    public void onThinking(Session session, ThingkingAssistantContent thingkingAssistantContent) {
    }

    @Override
    public void onToolUse(Session session, ToolUseAssistantContent toolUseAssistantContent) {
    }

    @Override
    public void onToolResult(Session session, ToolResultAssistantContent toolResultAssistantContent) {
    }

    @Override
    public void onOtherContent(Session session, AssistantContent<?> other) {
    }

    @Override
    public void onUsage(Session session, AssistantUsage AssistantUsage) {
    }
}
