package com.alibaba.qwen.code.cli;

import java.util.List;

import com.alibaba.qwen.code.cli.protocol.message.Message;
import com.alibaba.qwen.code.cli.protocol.message.assistant.SDKAssistantMessage;

import org.junit.jupiter.api.Test;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import static org.junit.jupiter.api.Assertions.*;

class QwenCliTest {

    private static final Logger log = LoggerFactory.getLogger(QwenCliTest.class);
    @Test
    void query() {
        List<Message> result = QwenCli.query("hello world");
        log.info("result: {}", result);
        assertNotNull(result);
    }
}
