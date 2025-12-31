package com.alibaba.qwen.code.cli;

import java.util.List;

import org.junit.jupiter.api.Test;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import static org.junit.jupiter.api.Assertions.*;

class QwenCodeCliTest {

    private static final Logger log = LoggerFactory.getLogger(QwenCodeCliTest.class);
    @Test
    void simpleQuery() {
        List<String> result = QwenCodeCli.simpleQuery("hello world");
        log.info("result: {}", result);
        assertNotNull(result);
    }
}
