package com.alibaba.qwen.code.cli.session.event;

import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;

public class SessionEventSimpleConsumers implements SessionEventConsumers {
    @Override
    public void onSystemMessage(SDKSystemMessage systemMessage) {
    }

    @Override
    public void onResultMessage(SDKResultMessage resultMessage) {
    }

    @Override
    public void onAssistantMessage(SDKAssistantMessage assistantMessage) {
    }

    @Override
    public void onOtherMessage(String message) {
    }
}
