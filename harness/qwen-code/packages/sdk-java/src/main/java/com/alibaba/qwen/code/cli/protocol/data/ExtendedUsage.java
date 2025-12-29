package com.alibaba.qwen.code.cli.protocol.data;

import com.alibaba.fastjson2.annotation.JSONField;

public class ExtendedUsage extends Usage {
    @JSONField(name = "server_tool_use")
    private ServerToolUse serverToolUse;

    @JSONField(name = "service_tier")
    private String serviceTier;

    @JSONField(name = "cache_creation")
    private CacheCreation cacheCreation;

    public ServerToolUse getServerToolUse() {
        return serverToolUse;
    }

    public void setServerToolUse(ServerToolUse serverToolUse) {
        this.serverToolUse = serverToolUse;
    }

    public String getServiceTier() {
        return serviceTier;
    }

    public void setServiceTier(String serviceTier) {
        this.serviceTier = serviceTier;
    }

    public CacheCreation getCacheCreation() {
        return cacheCreation;
    }

    public void setCacheCreation(CacheCreation cacheCreation) {
        this.cacheCreation = cacheCreation;
    }

    public static class ServerToolUse {
        @JSONField(name = "web_search_requests")
        private int webSearchRequests;
    }

    public static class CacheCreation {
        @JSONField(name = "ephemeral_1h_input_tokens")
        private int ephemeral1hInputTokens;

        @JSONField(name = "ephemeral_5m_input_tokens")
        private int ephemeral5mInputTokens;

        public int getEphemeral1hInputTokens() {
            return ephemeral1hInputTokens;
        }

        public void setEphemeral1hInputTokens(int ephemeral1hInputTokens) {
            this.ephemeral1hInputTokens = ephemeral1hInputTokens;
        }

        public int getEphemeral5mInputTokens() {
            return ephemeral5mInputTokens;
        }

        public void setEphemeral5mInputTokens(int ephemeral5mInputTokens) {
            this.ephemeral5mInputTokens = ephemeral5mInputTokens;
        }
    }
}
