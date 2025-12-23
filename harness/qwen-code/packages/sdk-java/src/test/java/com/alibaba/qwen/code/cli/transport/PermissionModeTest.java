package com.alibaba.qwen.code.cli.transport;

import org.junit.Assert;
import org.junit.Test;

public class PermissionModeTest {

    @Test
    public void shouldBeReturnQwenPermissionModeValue() {
        Assert.assertEquals("default", PermissionMode.DEFAULT.getValue());
        Assert.assertEquals("plan", PermissionMode.PLAN.getValue());
        Assert.assertEquals("auto-edit", PermissionMode.AUTO_EDIT.getValue());
        Assert.assertEquals("yolo", PermissionMode.YOLO.getValue());
    }

}
