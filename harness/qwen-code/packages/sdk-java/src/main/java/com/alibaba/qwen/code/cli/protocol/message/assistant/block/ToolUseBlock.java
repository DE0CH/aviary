package com.alibaba.qwen.code.cli.protocol.message.assistant.block;

import java.util.List;
import java.util.Map;

import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(typeKey = "type", typeName = "tool_use")
public class ToolUseBlock extends ContentBlock {
    private String id;
    private String name;
    private Map<String, Object> input;
    private List<Annotation> annotations;

    // 构造函数
    public ToolUseBlock() {}

    public String getId() {
        return id;
    }

    public void setId(String id) {
        this.id = id;
    }

    public String getName() {
        return name;
    }

    public void setName(String name) {
        this.name = name;
    }

    public Map<String, Object> getInput() {
        return input;
    }

    public void setInput(Map<String, Object> input) {
        this.input = input;
    }

    public List<Annotation> getAnnotations() {
        return annotations;
    }

    public void setAnnotations(List<Annotation> annotations) {
        this.annotations = annotations;
    }
}
