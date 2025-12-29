package com.alibaba.qwen.code.cli;

import java.util.ArrayList;
import java.util.List;

import com.alibaba.qwen.code.cli.protocol.message.Message;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.transport.Transport;
import com.alibaba.qwen.code.cli.transport.process.ProcessTransport;

public class QwenCli {
    public static List<Message> query(String prompt) {
        Transport transport;
        try {
            transport = new ProcessTransport();
        } catch (Exception e) {
            throw new RuntimeException("initialized ProcessTransport error!", e);
        }

        Session session;
        try {
            session = new Session(transport);
        } catch (Exception e) {
            throw new RuntimeException("initialized Session error!", e);
        }

        final List<Message> response = new ArrayList<>();
        try {
            session.sendPrompt(prompt, new SessionEventSimpleConsumers() {
                @Override
                public void onSystemMessage(SDKSystemMessage systemMessage) {
                    response.add(systemMessage);
                }

                @Override
                public void onAssistantMessage(SDKAssistantMessage assistantMessage) {
                    response.add(assistantMessage);
                }
            });
        } catch (Exception e) {
            throw new RuntimeException("sendPrompt error!", e);
        }

        try {
            session.close();
        } catch (Exception e) {
            throw new RuntimeException("close Session error!", e);
        }
        return response;
    }
}
