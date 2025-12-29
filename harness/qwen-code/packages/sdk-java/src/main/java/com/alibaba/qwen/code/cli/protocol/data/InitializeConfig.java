package com.alibaba.qwen.code.cli.protocol.data;

public class InitializeConfig {
    String hooks;
    String sdkMcpServers;
    String mcpServers;
    String agents;

    public String getHooks() {
        return hooks;
    }

    public void setHooks(String hooks) {
        this.hooks = hooks;
    }

    public String getSdkMcpServers() {
        return sdkMcpServers;
    }

    public void setSdkMcpServers(String sdkMcpServers) {
        this.sdkMcpServers = sdkMcpServers;
    }

    public String getMcpServers() {
        return mcpServers;
    }

    public void setMcpServers(String mcpServers) {
        this.mcpServers = mcpServers;
    }

    public String getAgents() {
        return agents;
    }

    public void setAgents(String agents) {
        this.agents = agents;
    }
}
