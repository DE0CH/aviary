package com.alibaba.qwen.code.cli.protocol.message.assistant.event;

import com.alibaba.fastjson2.annotation.JSONField;
import com.alibaba.fastjson2.annotation.JSONType;
import com.alibaba.qwen.code.cli.protocol.data.AssistantContent;

@JSONType(typeKey = "type", typeName = "content_block_delta")
public class ContentBlockDeltaEvent extends StreamEvent {
    private int index;
    private ContentBlockDelta delta;

    public int getIndex() {
        return index;
    }

    public void setIndex(int index) {
        this.index = index;
    }

    public ContentBlockDelta getDelta() {
        return delta;
    }

    public void setDelta(ContentBlockDelta delta) {
        this.delta = delta;
    }

    @JSONType(typeKey = "type", typeName = "ContentBlockDelta",
            seeAlso = {ContentBlockDeltaText.class, ContentBlockDeltaThinking.class, ContentBlockDeltaInputJson.class})
    public abstract static class ContentBlockDelta implements AssistantContent {
        private String type;

        public String getType() {
            return type;
        }

        public void setType(String type) {
            this.type = type;
        }
    }

    @JSONType(typeKey = "type", typeName = "text_delta")
    public static class ContentBlockDeltaText extends ContentBlockDelta {
        private String text;

        public String getText() {
            return text;
        }

        public void setText(String text) {
            this.text = text;
        }

        @Override
        public Object getContent() {
            return text;
        }
    }

    @JSONType(typeKey = "type", typeName = "thinking_delta")
    public static class ContentBlockDeltaThinking extends ContentBlockDelta {
        private String thinking;

        public String getThinking() {
            return thinking;
        }

        public void setThinking(String thinking) {
            this.thinking = thinking;
        }

        @Override
        public Object getContent() {
            return thinking;
        }
    }

    @JSONType(typeKey = "type", typeName = "input_json_delta")
    public static class ContentBlockDeltaInputJson extends ContentBlockDelta {
        @JSONField(name = "partial_json")
        private String partialJson;

        public String getPartialJson() {
            return partialJson;
        }

        public void setPartialJson(String partialJson) {
            this.partialJson = partialJson;
        }

        @Override
        public Object getContent() {
            return partialJson;
        }
    }
}
