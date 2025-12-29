package com.alibaba.qwen.code.cli.protocol.message;

import java.util.List;
import java.util.Map;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(typeKey = "type", typeName = "system")
public class SDKSystemMessage extends MessageBase {
    private String subtype;
    private String uuid;
    @JSONField(name = "session_id")
    private String sessionId;
    private Object data;
    private String cwd;
    private List<String> tools;
    @JSONField(name = "mcp_servers")
    private List<McpServer> mcpServers;
    private String model;
    @JSONField(name = "permission_mode")
    private String permissionMode;
    @JSONField(name = "slash_commands")
    private List<String> slashCommands;
    @JSONField(name = "qwen_code_version")
    private String qwenCodeVersion;
    @JSONField(name = "output_style")
    private String outputStyle;
    private List<String> agents;
    private List<String> skills;
    private Map<String, Object> capabilities;
    @JSONField(name = "compact_metadata")
    private CompactMetadata compactMetadata;

    public SDKSystemMessage() {
        super();
        this.type = "system";
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

    public Object getData() {
        return data;
    }

    public void setData(Object data) {
        this.data = data;
    }

    public String getCwd() {
        return cwd;
    }

    public void setCwd(String cwd) {
        this.cwd = cwd;
    }

    public List<String> getTools() {
        return tools;
    }

    public void setTools(List<String> tools) {
        this.tools = tools;
    }

    public List<McpServer> getMcpServers() {
        return mcpServers;
    }

    public void setMcpServers(List<McpServer> mcpServers) {
        this.mcpServers = mcpServers;
    }

    public String getModel() {
        return model;
    }

    public void setModel(String model) {
        this.model = model;
    }

    public String getPermissionMode() {
        return permissionMode;
    }

    public void setPermissionMode(String permissionMode) {
        this.permissionMode = permissionMode;
    }

    public List<String> getSlashCommands() {
        return slashCommands;
    }

    public void setSlashCommands(List<String> slashCommands) {
        this.slashCommands = slashCommands;
    }

    public String getQwenCodeVersion() {
        return qwenCodeVersion;
    }

    public void setQwenCodeVersion(String qwenCodeVersion) {
        this.qwenCodeVersion = qwenCodeVersion;
    }

    public String getOutputStyle() {
        return outputStyle;
    }

    public void setOutputStyle(String outputStyle) {
        this.outputStyle = outputStyle;
    }

    public List<String> getAgents() {
        return agents;
    }

    public void setAgents(List<String> agents) {
        this.agents = agents;
    }

    public List<String> getSkills() {
        return skills;
    }

    public void setSkills(List<String> skills) {
        this.skills = skills;
    }

    public Map<String, Object> getCapabilities() {
        return capabilities;
    }

    public void setCapabilities(Map<String, Object> capabilities) {
        this.capabilities = capabilities;
    }

    public CompactMetadata getCompactMetadata() {
        return compactMetadata;
    }

    public void setCompactMetadata(CompactMetadata compactMetadata) {
        this.compactMetadata = compactMetadata;
    }

    public static class McpServer {
        private String name;
        private String status;

        // Getters and setters
        public String getName() {
            return name;
        }

        public void setName(String name) {
            this.name = name;
        }

        public String getStatus() {
            return status;
        }

        public void setStatus(String status) {
            this.status = status;
        }
    }

    public static class CompactMetadata {
        private String trigger;

        @JSONField(name = "pre_tokens")
        private Integer preTokens;

        // Getters and setters
        public String getTrigger() {
            return trigger;
        }

        public void setTrigger(String trigger) {
            this.trigger = trigger;
        }

        public Integer getPreTokens() {
            return preTokens;
        }

        public void setPreTokens(Integer preTokens) {
            this.preTokens = preTokens;
        }
    }
}
