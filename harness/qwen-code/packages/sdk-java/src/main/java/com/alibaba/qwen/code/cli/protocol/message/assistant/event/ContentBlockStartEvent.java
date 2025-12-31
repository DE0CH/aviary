package com.alibaba.qwen.code.cli.protocol.message.assistant.event;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;
import com.alibaba.qwen.code.cli.protocol.message.assistant.block.ContentBlock;

@JSONType(typeKey = "type", typeName = "content_block_start")
public class ContentBlockStartEvent extends StreamEvent{
    private int index;

    @JSONField(name = "content_block")
    private ContentBlock contentBlock;
}
