package com.alibaba.qwen.code.cli.session;

import com.alibaba.fastjson2.JSON;
import com.alibaba.fastjson2.JSONObject;
import com.alibaba.fastjson2.TypeReference;
import com.alibaba.qwen.code.cli.protocol.data.Capabilities;
import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKUserMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlInitializeRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlInitializeResponse;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlResponse;
import com.alibaba.qwen.code.cli.session.event.SessionEventConsumers;
import com.alibaba.qwen.code.cli.session.exception.SessionCloseException;
import com.alibaba.qwen.code.cli.session.exception.SessionSendPromptException;
import com.alibaba.qwen.code.cli.session.exception.SessionStartException;
import com.alibaba.qwen.code.cli.transport.Transport;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public class Session {
    private final Transport transport;
    private Capabilities capabilities;
    private static final Logger log = LoggerFactory.getLogger(Session.class);

    public Session(Transport transport) throws SessionStartException {
        if (transport == null || !transport.isAvailable()) {
            throw new SessionStartException("Transport is not available");
        }
        this.transport = transport;
        start();
    }

    private void start() throws SessionStartException {
        try {
            String response = transport.inputWaitForOneLine(CLIControlRequest.create(new CLIControlInitializeRequest()).toString());
            CLIControlResponse<CLIControlInitializeResponse> cliControlResponse = JSON.parseObject(response, new TypeReference<CLIControlResponse<CLIControlInitializeResponse>>() {});
            this.capabilities = cliControlResponse.getResponse().getResponse().getCapabilities();
        } catch (Exception e) {
            throw new SessionStartException("Failed to initialize the session", e);
        }
    }

    public void close() throws SessionCloseException {
        try {
            transport.close();
        } catch (Exception e) {
            throw new SessionCloseException("Failed to close the session", e);
        }
    }

    public Capabilities getCapabilities() {
        return capabilities;
    }

    public void sendPrompt(String prompt, SessionEventConsumers sessionEventConsumers) throws SessionSendPromptException {
        if (!transport.isAvailable()) {
            throw new SessionSendPromptException("Session is not available");
        }

        try {
            transport.inputWaitForMultiLine(new SDKUserMessage().setContent(prompt).toString(), (line) -> {
                log.debug("read a message from agent {}", line);
                JSONObject jsonObject = JSON.parseObject(line);

                String messageType = jsonObject.getString("type");
                if ("system".equals(messageType)) {
                    sessionEventConsumers.onSystemMessage(JSON.parseObject(line, SDKSystemMessage.class));
                    return false;
                } else if ("assistant".equals(messageType)) {
                    sessionEventConsumers.onAssistantMessage(JSON.parseObject(line, SDKAssistantMessage.class));
                    return false;
                } else if ("result".equals(messageType)) {
                    sessionEventConsumers.onResultMessage(JSON.parseObject(line, SDKResultMessage.class));
                    return true;
                } else {
                    log.warn("unknown message type: {}", messageType);
                    sessionEventConsumers.onOtherMessage(line);
                    return false;
                }
            });
        } catch (Exception e) {
            throw new SessionSendPromptException("Failed to send prompt", e);
        }
    }
}
