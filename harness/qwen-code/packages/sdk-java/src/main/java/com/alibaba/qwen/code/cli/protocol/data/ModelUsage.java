package com.alibaba.qwen.code.cli.protocol.data;

public class ModelUsage {
    private int inputTokens;
    private int outputTokens;
    private int cacheReadInputTokens;
    private int cacheCreationInputTokens;
    private int webSearchRequests;
    private int contextWindow;

    public int getInputTokens() {
        return inputTokens;
    }

    public void setInputTokens(int inputTokens) {
        this.inputTokens = inputTokens;
    }

    public int getOutputTokens() {
        return outputTokens;
    }

    public void setOutputTokens(int outputTokens) {
        this.outputTokens = outputTokens;
    }

    public int getCacheReadInputTokens() {
        return cacheReadInputTokens;
    }

    public void setCacheReadInputTokens(int cacheReadInputTokens) {
        this.cacheReadInputTokens = cacheReadInputTokens;
    }

    public int getCacheCreationInputTokens() {
        return cacheCreationInputTokens;
    }

    public void setCacheCreationInputTokens(int cacheCreationInputTokens) {
        this.cacheCreationInputTokens = cacheCreationInputTokens;
    }

    public int getWebSearchRequests() {
        return webSearchRequests;
    }

    public void setWebSearchRequests(int webSearchRequests) {
        this.webSearchRequests = webSearchRequests;
    }

    public int getContextWindow() {
        return contextWindow;
    }

    public void setContextWindow(int contextWindow) {
        this.contextWindow = contextWindow;
    }
}
