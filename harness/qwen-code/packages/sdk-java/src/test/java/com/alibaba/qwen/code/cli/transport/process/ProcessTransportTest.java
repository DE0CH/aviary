package com.alibaba.qwen.code.cli.transport.process;

import java.io.IOException;

import com.alibaba.qwen.code.cli.transport.TransportOptions;

import org.junit.jupiter.api.Test;

class ProcessTransportTest {

    @Test
    void shouldStartAndCloseSuccessfully() throws IOException {
        TransportOptions transportOptions = new TransportOptions();
        ProcessTransport processTransport = new ProcessTransport(transportOptions);
        processTransport.close();
    }

}
