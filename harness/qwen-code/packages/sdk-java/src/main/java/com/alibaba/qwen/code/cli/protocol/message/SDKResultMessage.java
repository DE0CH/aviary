package com.alibaba.qwen.code.cli.protocol.message;

import java.util.List;
import java.util.Map;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;
import com.alibaba.qwen.code.cli.protocol.data.CLIPermissionDenial;
import com.alibaba.qwen.code.cli.protocol.data.ExtendedUsage;
import com.alibaba.qwen.code.cli.protocol.data.Usage;

@JSONType(typeKey = "type", typeName = "result")
public class SDKResultMessage extends MessageBase {
    private String subtype; // 'error_max_turns' | 'error_during_execution'
    private String uuid;

    @JSONField(name = "session_id")
    private String sessionId;

    @JSONField(name = "is_error")
    private boolean isError = true;

    @JSONField(name = "duration_ms")
    private Long durationMs;

    @JSONField(name = "duration_api_ms")
    private Long durationApiMs;

    @JSONField(name = "num_turns")
    private Integer numTurns;
    private ExtendedUsage usage;
    private Map<String, Usage> modelUsage;

    @JSONField(name = "permission_denials")
    private List<CLIPermissionDenial> permissionDenials;
    private Error error;

    public SDKResultMessage() {
        super();
        this.type = "result";
    }

    public String getSubtype() {
        return subtype;
    }

    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }

    public String getUuid() {
        return uuid;
    }

    public void setUuid(String uuid) {
        this.uuid = uuid;
    }

    public String getSessionId() {
        return sessionId;
    }

    public void setSessionId(String sessionId) {
        this.sessionId = sessionId;
    }

    public boolean isError() {
        return isError;
    }

    public void setError(boolean error) {
        isError = error;
    }

    public Long getDurationMs() {
        return durationMs;
    }

    public void setDurationMs(Long durationMs) {
        this.durationMs = durationMs;
    }

    public Long getDurationApiMs() {
        return durationApiMs;
    }

    public void setDurationApiMs(Long durationApiMs) {
        this.durationApiMs = durationApiMs;
    }

    public Integer getNumTurns() {
        return numTurns;
    }

    public void setNumTurns(Integer numTurns) {
        this.numTurns = numTurns;
    }

    public ExtendedUsage getUsage() {
        return usage;
    }

    public void setUsage(ExtendedUsage usage) {
        this.usage = usage;
    }

    public Map<String, Usage> getModelUsage() {
        return modelUsage;
    }

    public void setModelUsage(Map<String, Usage> modelUsage) {
        this.modelUsage = modelUsage;
    }

    public List<CLIPermissionDenial> getPermissionDenials() {
        return permissionDenials;
    }

    public void setPermissionDenials(List<CLIPermissionDenial> permissionDenials) {
        this.permissionDenials = permissionDenials;
    }

    public Error getError() {
        return error;
    }

    public void setError(Error error) {
        this.error = error;
    }

    public static class Error {
        private String type;
        private String message;

        public String getType() {
            return type;
        }

        public void setType(String type) {
            this.type = type;
        }

        public String getMessage() {
            return message;
        }

        public void setMessage(String message) {
            this.message = message;
        }
    }
}
