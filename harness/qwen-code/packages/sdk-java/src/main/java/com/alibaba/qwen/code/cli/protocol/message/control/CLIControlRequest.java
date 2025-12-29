package com.alibaba.qwen.code.cli.protocol.message.control;

import java.util.UUID;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;
import com.alibaba.qwen.code.cli.protocol.message.MessageBase;

@JSONType(typeKey = "type", typeName = "control_request")
public class CLIControlRequest<R> extends MessageBase {
    @JSONField(name = "request_id")
    private String requestId = UUID.randomUUID().toString();

    private R request;

    public CLIControlRequest() {
        super();
        type = "control_request";
    }

    public static <T> CLIControlRequest<T> create(T request) {
        CLIControlRequest<T> controlRequest = new CLIControlRequest<>();
        controlRequest.setRequest(request);
        return controlRequest;
    }

    public String getRequestId() {
        return requestId;
    }

    public CLIControlRequest<R> setRequestId(String requestId) {
        this.requestId = requestId;
        return this;
    }

    public R getRequest() {
        return request;
    }

    public CLIControlRequest<R> setRequest(R request) {
        this.request = request;
        return this;
    }
}
