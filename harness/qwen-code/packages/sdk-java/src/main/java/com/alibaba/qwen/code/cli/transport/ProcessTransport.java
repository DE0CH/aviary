package com.alibaba.qwen.code.cli.transport;

public class ProcessTransport {
    Process process;
    TransportOptions transportOptions;

    public ProcessTransport(TransportOptions transportOptions) {
        this.transportOptions = transportOptions;
    }
}
