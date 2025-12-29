package com.alibaba.qwen.code.cli.session;

import java.io.IOException;

import com.alibaba.fastjson2.JSON;
import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.session.event.SessionEventConsumers;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.session.exception.SessionCloseException;
import com.alibaba.qwen.code.cli.session.exception.SessionSendPromptException;
import com.alibaba.qwen.code.cli.session.exception.SessionStartException;
import com.alibaba.qwen.code.cli.transport.Transport;
import com.alibaba.qwen.code.cli.transport.process.ProcessTransport;

import org.junit.jupiter.api.Test;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

class SessionTest {

    private static final Logger log = LoggerFactory.getLogger(SessionTest.class);
    @Test
    void sendPrompt() throws IOException, SessionStartException, SessionSendPromptException, SessionCloseException {
        Transport transport = new ProcessTransport();
        Session session = new Session(transport);
        session.sendPrompt("hello world", new SessionEventSimpleConsumers() {
            @Override
            public void onSystemMessage(SDKSystemMessage systemMessage) {
                log.info("systemMessage: {}", systemMessage);
            }

            @Override
            public void onResultMessage(SDKResultMessage resultMessage) {
                log.info("resultMessage: {}", resultMessage);
            }

            @Override
            public void onAssistantMessage(SDKAssistantMessage assistantMessage) {
                log.info("assistantMessage: {}", assistantMessage);
            }

            @Override
            public void onOtherMessage(String message) {
                log.info("otherMessage: {}", message);
            }
        });
        session.close();
    }

    @Test
    void testJSON() {
        String json = "{\"type\":\"assistant\",\"uuid\":\"ed8374fe-a4eb-4fc0-9780-9bd2fd831cda\",\"session_id\":\"166badc0-e6d3-4978-ae47-4ccd51c468ef\",\"message\":{\"content\":[{\"text\":\"Hello! How can I help you with the Qwen Code SDK for Java today?\",\"type\":\"text\"}],\"id\":\"ed8374fe-a4eb-4fc0-9780-9bd2fd831cda\",\"model\":\"qwen3-coder-plus\",\"role\":\"assistant\",\"type\":\"message\",\"usage\":{\"cache_read_input_tokens\":12766,\"input_tokens\":12770,\"output_tokens\":17,\"total_tokens\":12787}}}";
        SDKAssistantMessage assistantMessage = JSON.parseObject(json, SDKAssistantMessage.class);
        log.info("the assistantMessage: {}", assistantMessage);
    }
}
