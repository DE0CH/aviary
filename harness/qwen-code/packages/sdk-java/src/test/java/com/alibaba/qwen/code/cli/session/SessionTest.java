package com.alibaba.qwen.code.cli.session;

import java.io.IOException;
import java.util.List;

import com.alibaba.fastjson2.JSON;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;
import com.alibaba.qwen.code.cli.protocol.data.PermissionMode;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Allow;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior;
import com.alibaba.qwen.code.cli.protocol.data.behavior.Behavior.Operation;
import com.alibaba.qwen.code.cli.protocol.message.SDKResultMessage;
import com.alibaba.qwen.code.cli.protocol.message.SDKSystemMessage;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlPermissionRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlRequest;
import com.alibaba.qwen.code.cli.protocol.message.control.CLIControlResponse;
import com.alibaba.qwen.code.cli.session.event.SessionEventConsumers;
import com.alibaba.qwen.code.cli.session.event.SessionEventSimpleConsumers;
import com.alibaba.qwen.code.cli.session.exception.SessionControlException;
import com.alibaba.qwen.code.cli.session.exception.SessionSendPromptException;
import com.alibaba.qwen.code.cli.transport.Transport;
import com.alibaba.qwen.code.cli.transport.TransportOptions;
import com.alibaba.qwen.code.cli.transport.process.ProcessTransport;

import org.apache.commons.lang3.StringUtils;
import org.junit.jupiter.api.Test;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

class SessionTest {

    private static final Logger log = LoggerFactory.getLogger(SessionTest.class);

    @Test
    void partialSendPromptSuccessfully() throws IOException, SessionControlException, SessionSendPromptException {
        Transport transport = new ProcessTransport(new TransportOptions().setIncludePartialMessages(true));
        Session session = new Session(transport);
        session.sendPrompt("in the dir src/test/temp/, create file empty file test.touch", new SessionEventSimpleConsumers() {
            @Override
            public void onAssistantMessageIncludePartial(Session session, List<AssistantContent> assistantContents,
                    AssistantMessageOutputType assistantMessageOutputType) {
                log.info("onAssistantMessageIncludePartial: {}", JSON.toJSONString(assistantContents));
            }
        }.setDefaultPermissionOperation(Operation.allow));
    }

    @Test
    void setPermissionModeSuccessfully() throws IOException, SessionControlException, SessionSendPromptException {
        Transport transport = new ProcessTransport();
        Session session = new Session(transport);

        log.info(session.setPermissionMode(PermissionMode.YOLO).map(s -> s ? "setPermissionMode 1 success" : "setPermissionMode 1 error")
                .orElse("setPermissionMode 1 unknown"));
        session.sendPrompt("in the dir src/test/temp/, create file empty file test.touch", new SessionEventSimpleConsumers());

        log.info(session.setPermissionMode(PermissionMode.PLAN).map(s -> s ? "setPermissionMode 2 success" : "setPermissionMode 2 error")
                .orElse("setPermissionMode 2 unknown"));
        session.sendPrompt("rename test.touch to test_rename.touch", new SessionEventSimpleConsumers());

        log.info(session.setPermissionMode(PermissionMode.AUTO_EDIT).map(s -> s ? "setPermissionMode 3 success" : "setPermissionMode 3 error")
                .orElse("setPermissionMode 3 unknown"));
        session.sendPrompt("rename test.touch to test_rename.touch", new SessionEventSimpleConsumers());

        session.sendPrompt("rename test.touch to test_rename.touch again user will allow", new SessionEventSimpleConsumers() {
            public Behavior onPermissionRequest(Session session, CLIControlRequest<CLIControlPermissionRequest> permissionRequest) {
                log.info("permissionRequest: {}", permissionRequest);
                return new Allow().setUpdatedInput(permissionRequest.getRequest().getInput());
            }
        });

        session.close();
    }

    @Test
    void sendPromptAndSetModelSuccessfully() throws IOException, SessionControlException, SessionSendPromptException {
        Transport transport = new ProcessTransport();
        Session session = new Session(transport);

        log.info(session.setModel("qwen3-coder-flash").map(s -> s ? "setModel 1 success" : "setModel 1 error").orElse("setModel 1 unknown"));
        writeSplitLine("setModel 1 end");

        session.sendPrompt("hello world", new SessionEventSimpleConsumers());
        writeSplitLine("prompt 1 end");

        log.info(session.setModel("qwen3-coder-plus").map(s -> s ? "setModel 2 success" : "setModel 2 error").orElse("setModel 2 unknown"));
        writeSplitLine("setModel 1 end");

        session.sendPrompt("查看下当前目录有多少个文件", new SessionEventSimpleConsumers());
        writeSplitLine("prompt 2 end");

        log.info(session.setModel("qwen3-max").map(s -> s ? "setModel 3 success" : "setModel 3 error").orElse("setModel 3 unknown"));
        writeSplitLine("setModel 1 end");

        session.sendPrompt("查看下当前目录有多少个xml文件", new SessionEventSimpleConsumers());
        writeSplitLine("prompt 3 end");

        session.close();
    }

    @Test
    void sendPromptAndInterruptContinueSuccessfully() throws IOException, SessionControlException, SessionSendPromptException {
        Transport transport = new ProcessTransport();
        Session session = new Session(transport);

        SessionEventConsumers sessionEventConsumers = new SessionEventSimpleConsumers() {
            @Override
            public void onSystemMessage(Session session, SDKSystemMessage systemMessage) {
                log.info("systemMessage: {}", systemMessage);
            }

            @Override
            public void onResultMessage(Session session, SDKResultMessage resultMessage) {
                log.info("resultMessage: {}", resultMessage);
            }

            @Override
            public void onAssistantMessage(Session session, SDKAssistantMessage assistantMessage) {
                log.info("assistantMessage: {}", assistantMessage);
                try {
                    session.interrupt();
                } catch (SessionControlException e) {
                    log.error("interrupt error", e);
                }
            }

            @Override
            public void onControlResponse(Session session, CLIControlResponse<?> cliControlResponse) {
                log.info("cliControlResponse: {}", cliControlResponse);
            }

            @Override
            public void onOtherMessage(Session session, String message) {
                log.info("otherMessage: {}", message);
            }
        };
        session.sendPrompt("查看下当前目录有多少个文件", sessionEventConsumers);
        writeSplitLine("prompt 1 end");

        session.continueSession();
        session.sendPrompt("hello world", sessionEventConsumers);
        writeSplitLine("prompt 2 end");

        session.continueSession();
        session.sendPrompt("当前目录有多少个java文件", sessionEventConsumers);
        writeSplitLine("prompt 3 end");

        session.close();
    }

    public void writeSplitLine(String line) {
        log.info("{}  {}", line, StringUtils.repeat("=", 300));
    }

    @Test
    void testJSON() {
        String json
                = "{\"type\":\"assistant\",\"uuid\":\"ed8374fe-a4eb-4fc0-9780-9bd2fd831cda\","
                + "\"session_id\":\"166badc0-e6d3-4978-ae47-4ccd51c468ef\",\"message\":{\"content\":[{\"text\":\"Hello! How can I help you with the"
                + " Qwen Code SDK for Java today?\",\"type\":\"text\"}],\"id\":\"ed8374fe-a4eb-4fc0-9780-9bd2fd831cda\","
                + "\"model\":\"qwen3-coder-plus\",\"role\":\"assistant\",\"type\":\"message\",\"usage\":{\"cache_read_input_tokens\":12766,"
                + "\"input_tokens\":12770,\"output_tokens\":17,\"total_tokens\":12787}}}";
        SDKAssistantMessage assistantMessage = JSON.parseObject(json, SDKAssistantMessage.class);
        log.info("the assistantMessage: {}", assistantMessage);
    }
}
