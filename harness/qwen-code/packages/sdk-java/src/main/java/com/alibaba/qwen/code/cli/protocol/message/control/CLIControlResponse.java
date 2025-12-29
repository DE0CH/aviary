package com.alibaba.qwen.code.cli.protocol.message.control;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;
import com.alibaba.qwen.code.cli.protocol.message.MessageBase;

@JSONType(typeKey = "type", typeName = "control_response")
public class CLIControlResponse<R> extends MessageBase {
    private Response<R> response;

    public CLIControlResponse() {
        super();
        this.type = "control_response";
    }

    public Response<R> getResponse() {
        return response;
    }

    public void setResponse(Response<R> response) {
        this.response = response;
    }

    public static class Response<R> {
        @JSONField(name = "request_id")
        private String requestId;
        private String subtype;
        R response;

        public String getRequestId() {
            return requestId;
        }

        public void setRequestId(String requestId) {
            this.requestId = requestId;
        }

        public String getSubtype() {
            return subtype;
        }

        public void setSubtype(String subtype) {
            this.subtype = subtype;
        }

        public R getResponse() {
            return response;
        }

        public void setResponse(R response) {
            this.response = response;
        }
    }
}
