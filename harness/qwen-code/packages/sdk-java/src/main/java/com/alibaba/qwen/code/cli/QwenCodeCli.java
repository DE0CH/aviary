package com.alibaba.qwen.code.cli;

import java.util.ArrayList;
import java.util.List;
import java.util.function.Consumer;
import java.util.stream.Collectors;

import com.alibaba.fastjson2.JSON;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior.Operation;
import com.alibaba.qwen.code.cli.session.Session;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.transport.Transport;
import com.alibaba.qwen.code.cli.transport.TransportOptions;
import com.alibaba.qwen.code.cli.transport.process.ProcessTransport;
import com.alibaba.qwen.code.cli.utils.MyConcurrentUtils;
import com.alibaba.qwen.code.cli.utils.Timeout;

public class QwenCodeCli {
    public static List<String> simpleQuery(String prompt) {
        final List<String> response = new ArrayList<>();
        MyConcurrentUtils.runAndWait(() -> simpleQuery(prompt, response::add), Timeout.TIMEOUT_30_MINUTES);
        return response;
    }

    public static void simpleQuery(String prompt, Consumer<String> messageConsumer) {
        Session session = newSessionWithProcessTransport(new TransportOptions());
        try {
            session.sendPrompt(prompt, new SessionEventSimpleConsumers() {
                @Override
                public void onAssistantMessageIncludePartial(Session session, List<AssistantContent> assistantContents, AssistantMessageOutputType assistantMessageOutputType) {
                    messageConsumer.accept(assistantContents.stream()
                            .map(AssistantContent::getContent)
                            .map(content -> {
                                if (content instanceof String) {
                                    return (String) content;
                                } else {
                                    return JSON.toJSONString(content);
                                }
                            }).collect(Collectors.joining()));
                }
            }.setDefaultPermissionOperation(Operation.allow));
        } catch (Exception e) {
            throw new RuntimeException("sendPrompt error!", e);
        }

        try {
            session.close();
        } catch (Exception e) {
            throw new RuntimeException("close Session error!", e);
        }
    }

    public static Session newSessionWithProcessTransport(TransportOptions transportOptions) {
        Transport transport;
        try {
            transport = new ProcessTransport(transportOptions);
        } catch (Exception e) {
            throw new RuntimeException("initialized ProcessTransport error!", e);
        }

        Session session;
        try {
            session = new Session(transport);
        } catch (Exception e) {
            throw new RuntimeException("initialized Session error!", e);
        }
        return session;
    }
}
