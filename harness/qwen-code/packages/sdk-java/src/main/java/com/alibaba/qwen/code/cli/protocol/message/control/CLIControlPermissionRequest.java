package com.alibaba.qwen.code.cli.protocol.message.control;

import java.util.List;
import java.util.Map;

import com.alibaba.fastjson2.annotation.JSONField;

public class CLIControlPermissionRequest {
    private String subtype;

    @JSONField(name = "tool_name")
    private String toolName;

    @JSONField(name = "tool_use_id")
    private String toolUseId;

    private Map<String, Object> input;

    @JSONField(name = "permission_suggestions")
    private List<PermissionSuggestion> permissionSuggestions;

    @JSONField(name = "blocked_path")
    private String blockedPath;

    public String getSubtype() {
        return subtype;
    }

    public void setSubtype(String subtype) {
        this.subtype = subtype;
    }

    public String getToolName() {
        return toolName;
    }

    public void setToolName(String toolName) {
        this.toolName = toolName;
    }

    public String getToolUseId() {
        return toolUseId;
    }

    public void setToolUseId(String toolUseId) {
        this.toolUseId = toolUseId;
    }

    public Map<String, Object> getInput() {
        return input;
    }

    public void setInput(Map<String, Object> input) {
        this.input = input;
    }

    public List<PermissionSuggestion> getPermissionSuggestions() {
        return permissionSuggestions;
    }

    public void setPermissionSuggestions(
            List<PermissionSuggestion> permissionSuggestions) {
        this.permissionSuggestions = permissionSuggestions;
    }

    public String getBlockedPath() {
        return blockedPath;
    }

    public void setBlockedPath(String blockedPath) {
        this.blockedPath = blockedPath;
    }

    public static class PermissionSuggestion {
        private String type; // 'allow' | 'deny' | 'modify'
        private String label;
        private String description;
        private Object modifiedInput;

        public String getType() {
            return type;
        }

        public void setType(String type) {
            this.type = type;
        }

        public String getLabel() {
            return label;
        }

        public void setLabel(String label) {
            this.label = label;
        }

        public String getDescription() {
            return description;
        }

        public void setDescription(String description) {
            this.description = description;
        }

        public Object getModifiedInput() {
            return modifiedInput;
        }

        public void setModifiedInput(Object modifiedInput) {
            this.modifiedInput = modifiedInput;
        }
    }
}
