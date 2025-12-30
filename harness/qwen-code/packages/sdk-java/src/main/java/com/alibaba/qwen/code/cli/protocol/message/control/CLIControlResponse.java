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

    public Response<R> createResponse() {
        Response<R> response = new Response<>();
        this.setResponse(response);
        return response;
    }

    public static class Response<R> {
        @JSONField(name = "request_id")
        private String requestId;
        private String subtype = "success";
        R response;

        public String getRequestId() {
            return requestId;
        }

        public Response<R> setRequestId(String requestId) {
            this.requestId = requestId;
            return this;
        }

        public String getSubtype() {
            return subtype;
        }

        public Response<R> setSubtype(String subtype) {
            this.subtype = subtype;
            return this;
        }

        public R getResponse() {
            return response;
        }

        public Response<R> setResponse(R response) {
            this.response = response;
            return this;
        }
    }
}
