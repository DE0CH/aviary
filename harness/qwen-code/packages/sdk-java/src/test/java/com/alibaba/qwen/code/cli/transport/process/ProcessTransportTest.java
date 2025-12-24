package com.alibaba.qwen.code.cli.transport.process;

import java.io.IOException;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeoutException;

import com.alibaba.qwen.code.cli.transport.TransportOptions;

import org.junit.jupiter.api.Test;

class ProcessTransportTest {

    @Test
    void shouldStartAndCloseSuccessfully() throws IOException {
        TransportOptions transportOptions = new TransportOptions();
        ProcessTransport processTransport = new ProcessTransport(transportOptions);
        processTransport.close();
    }

    @Test
    void shouldInputWaitForOneLineSuccessfully() throws IOException, ExecutionException, InterruptedException, TimeoutException {
        TransportOptions transportOptions = new TransportOptions();
        ProcessTransport processTransport = new ProcessTransport(transportOptions);

        String message = "{\"type\": \"control_request\", \"request_id\": \"1\", \"request\": {\"subtype\": \"initialize\"} }";
        System.out.println(processTransport.inputWaitForOneLine(message));
    }

}
