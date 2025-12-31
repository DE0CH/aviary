package com.alibaba.qwen.code.cli.protocol.message.assistant.event;

import com.alibaba.fastjson2.annotation.JSONType;

@JSONType(typeName = "message_stop")
public class MessageStopStreamEvent extends StreamEvent{
}
