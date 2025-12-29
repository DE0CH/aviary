package com.alibaba.qwen.code.cli.session.event;

import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;

public interface SessionEventConsumers {
    void onSystemMessage(SDKSystemMessage systemMessage);

    void onResultMessage(SDKResultMessage resultMessage);

    void onAssistantMessage(SDKAssistantMessage assistantMessage);

    void onOtherMessage(String message);
}
