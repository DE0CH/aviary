package com.alibaba.qwen.code.cli.transport;

import java.io.IOException;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeoutException;
import java.util.function.Function;

public interface Transport {
    TransportOptions getTransportOptions();

    void start() throws IOException;

    void close() throws IOException;

    boolean isAvailable();

    String inputWaitForOneLine(String message) throws IOException, ExecutionException, InterruptedException, TimeoutException;

    void inputWaitForMultiLine(String message, Function<String, Boolean> callBackFunction) throws IOException;

    void inputNoWaitResponse(String message) throws IOException;
}
