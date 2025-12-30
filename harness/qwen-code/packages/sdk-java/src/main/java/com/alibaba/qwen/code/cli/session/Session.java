package com.alibaba.qwen.code.cli.session;

import java.io.IOException;
import java.util.Optional;

import com.alibaba.fastjson2.JSON;
import com.alibaba.fastjson2.JSONObject;
import com.alibaba.fastjson2.JSONReader.Feature;
import com.alibaba.fastjson2.TypeReference;
import com.alibaba.qwen.code.cli.protocol.data.Capabilities;
import com.alibaba.qwen.code.cli.protocol.data.PermissionMode;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Allow;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;
import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKUserMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlInitializeRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlInitializeResponse;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlInterruptRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlPermissionRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlPermissionResponse;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlResponse;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlSetModelRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlSetPermissionModeRequest;
import com.alibaba.qwen.code.cli.session.event.SessionEventConsumers;
import com.alibaba.qwen.code.cli.session.exception.SessionControlException;
import com.alibaba.qwen.code.cli.session.exception.SessionSendPromptException;
import com.alibaba.qwen.code.cli.transport.Transport;

import org.apache.commons.lang3.StringUtils;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public class Session {
    private final Transport transport;
    private CLIControlInitializeResponse lastCliControlInitializeResponse;
    private SDKSystemMessage lastSdkSystemMessage;
    private static final Logger log = LoggerFactory.getLogger(Session.class);

    public Session(Transport transport) throws SessionControlException {
        if (transport == null || !transport.isAvailable()) {
            throw new SessionControlException("Transport is not available");
        }
        this.transport = transport;
        start();
    }

    public void start() throws SessionControlException {
        try {
            if (!transport.isAvailable()) {
                transport.start();
            }
            String response = transport.inputWaitForOneLine(CLIControlRequest.create(new CLIControlInitializeRequest()).toString());
            CLIControlResponse<CLIControlInitializeResponse> cliControlResponse = JSON.parseObject(response,
                    new TypeReference<CLIControlResponse<CLIControlInitializeResponse>>() {});
            this.lastCliControlInitializeResponse = cliControlResponse.getResponse().getResponse();
        } catch (Exception e) {
            throw new SessionControlException("Failed to initialize the session", e);
        }
    }

    public void interrupt() throws SessionControlException {
        if (!isAvailable()) {
            throw new SessionControlException("Session is not available");
        }

        try {
            transport.inputNoWaitResponse(
                    new CLIControlRequest<CLIControlInterruptRequest>().setRequest(new CLIControlInterruptRequest()).toString());
        } catch (Exception e) {
            throw new SessionControlException("Failed to interrupt the session", e);
        }
    }

    public void setModel(String modelName) throws SessionControlException {
        if (!isAvailable()) {
            throw new SessionControlException("Session is not available");
        }

        CLIControlSetModelRequest cliControlSetModelRequest = new CLIControlSetModelRequest();
        cliControlSetModelRequest.setModel(modelName);
        try {
            transport.inputNoWaitResponse(new CLIControlRequest<CLIControlSetModelRequest>().setRequest(cliControlSetModelRequest).toString());
        } catch (Exception e) {
            throw new SessionControlException("Failed to set model", e);
        }
    }

    public void setPermissionMode(PermissionMode permissionMode) throws SessionControlException {
        if (!isAvailable()) {
            throw new SessionControlException("Session is not available");
        }

        CLIControlSetPermissionModeRequest cliControlSetPermissionModeRequest = new CLIControlSetPermissionModeRequest();
        cliControlSetPermissionModeRequest.setMode(permissionMode.getValue());
        try {
            transport.inputNoWaitResponse(
                    new CLIControlRequest<CLIControlSetPermissionModeRequest>().setRequest(cliControlSetPermissionModeRequest).toString());
        } catch (Exception e) {
            throw new SessionControlException("Failed to set model", e);
        }
    }

    public void continueSession() throws SessionControlException {
        resumeSession(getSessionId());
    }

    public void resumeSession(String sessionId) throws SessionControlException {
        if (!isAvailable()) {
            throw new SessionControlException("Session is not available");
        }

        if (StringUtils.isNotBlank(sessionId)) {
            transport.getTransportOptions().setResumeSessionId(sessionId);
        }
        this.start();
    }

    public String getSessionId() {
        return Optional.ofNullable(lastSdkSystemMessage).map(SDKSystemMessage::getSessionId).orElse(null);
    }

    public void close() throws SessionControlException {
        try {
            transport.close();
        } catch (Exception e) {
            throw new SessionControlException("Failed to close the session", e);
        }
    }

    public boolean isAvailable() {
        return transport.isAvailable();
    }

    public Capabilities getCapabilities() {
        return Optional.ofNullable(lastCliControlInitializeResponse).map(CLIControlInitializeResponse::getCapabilities).orElse(new Capabilities());
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
                    lastSdkSystemMessage = jsonObject.to(SDKSystemMessage.class);
                    sessionEventConsumers.onSystemMessage(this, lastSdkSystemMessage);
                    return false;
                } else if ("assistant".equals(messageType)) {
                    sessionEventConsumers.onAssistantMessage(this, jsonObject.to(SDKAssistantMessage.class));
                    return false;
                } else if ("user".equals(messageType)) {
                    sessionEventConsumers.onUserMessage(this, jsonObject.to(SDKUserMessage.class, Feature.FieldBased));
                    return false;
                } else if ("result".equals(messageType)) {
                    sessionEventConsumers.onResultMessage(this, jsonObject.to(SDKResultMessage.class));
                    return true;
                } else if ("control_response".equals(messageType)) {
                    sessionEventConsumers.onControlResponse(this, jsonObject.to(CLIControlResponse.class));
                    if (!"error".equals(jsonObject.getString("subtype"))) {
                        return false;
                    } else {
                        log.info("control_response error: {}", jsonObject.toJSONString());
                        return "error".equals(jsonObject.getString("subtype"));
                    }
                } else if ("control_request".equals(messageType)) {
                    return processControlRequest(jsonObject, sessionEventConsumers);
                } else {
                    log.warn("unknown message type: {}", messageType);
                    sessionEventConsumers.onOtherMessage(this, line);
                    return false;
                }
            });
        } catch (Exception e) {
            throw new SessionSendPromptException("Failed to send prompt", e);
        }
    }

    private boolean processControlRequest(JSONObject jsonObject, SessionEventConsumers sessionEventConsumers) {
        String subType = Optional.of(jsonObject)
                .map(cr -> cr.getJSONObject("request"))
                .map(r -> r.getString("subtype"))
                .orElse("");
        if ("can_use_tool".equals(subType)) {
            try {
                return processPermissionResponse(jsonObject, sessionEventConsumers);
            } catch (IOException e) {
                log.error("Failed to process permission response", e);
                return false;
            }
        } else {
            CLIControlResponse<?> cliControlResponse = sessionEventConsumers.onControlRequest(this,
                    jsonObject.to(new TypeReference<CLIControlRequest<?>>() {}));
            if (cliControlResponse != null) {
                try {
                    transport.inputNoWaitResponse(cliControlResponse.toString());
                } catch (Exception e) {
                    log.error("Failed to process control response", e);
                    return false;
                }
            }
            return false;
        }
    }

    private boolean processPermissionResponse(JSONObject jsonObject, SessionEventConsumers sessionEventConsumers) throws IOException {
        CLIControlRequest<CLIControlPermissionRequest> permissionRequest = jsonObject.to(new TypeReference<CLIControlRequest<CLIControlPermissionRequest>>() {});
        Behavior behavior = Optional.ofNullable(sessionEventConsumers.onPermissionRequest(this, permissionRequest))
                .map(b -> {
                    if (b instanceof Allow) {
                        Allow allow = (Allow) b;
                        if (allow.getUpdatedInput() == null) {
                            allow.setUpdatedInput(permissionRequest.getRequest().getInput());
                        }
                    }
                    return b;
                })
                .orElse(Behavior.defaultBehavior());
        CLIControlResponse<CLIControlPermissionResponse> permissionResponse = new CLIControlResponse<>();
        permissionResponse.createResponse().setResponse(new CLIControlPermissionResponse().setBehavior(behavior)).setRequestId(permissionRequest.getRequestId());
        String permissionMessage = permissionResponse.toString();
        log.debug("send permission message to agent: {}", permissionMessage);
        transport.inputNoWaitResponse(permissionMessage);

        return false;
    }
}
