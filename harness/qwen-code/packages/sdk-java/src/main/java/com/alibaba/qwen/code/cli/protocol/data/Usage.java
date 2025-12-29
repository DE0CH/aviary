package com.alibaba.qwen.code.cli.protocol.data;

import com.alibaba.fastjson2.annotation.JSONField;

public class Usage {
    @JSONField(name = "input_tokens")
    private Integer inputTokens;
    @JSONField(name = "output_tokens")
    private Integer outputTokens;
    @JSONField(name = "cache_creation_input_tokens")
    private Integer cacheCreationInputTokens;
    @JSONField(name = "cache_read_input_tokens")
    private Integer cacheReadInputTokens;
    @JSONField(name = "total_tokens")
    private Integer totalTokens;

    public Integer getInputTokens() {
        return inputTokens;
    }

    public void setInputTokens(Integer inputTokens) {
        this.inputTokens = inputTokens;
    }

    public Integer getOutputTokens() {
        return outputTokens;
    }

    public void setOutputTokens(Integer outputTokens) {
        this.outputTokens = outputTokens;
    }

    public Integer getCacheCreationInputTokens() {
        return cacheCreationInputTokens;
    }

    public void setCacheCreationInputTokens(Integer cacheCreationInputTokens) {
        this.cacheCreationInputTokens = cacheCreationInputTokens;
    }

    public Integer getCacheReadInputTokens() {
        return cacheReadInputTokens;
    }

    public void setCacheReadInputTokens(Integer cacheReadInputTokens) {
        this.cacheReadInputTokens = cacheReadInputTokens;
    }

    public Integer getTotalTokens() {
        return totalTokens;
    }

    public void setTotalTokens(Integer totalTokens) {
        this.totalTokens = totalTokens;
    }
}
