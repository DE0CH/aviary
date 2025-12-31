package com.alibaba.qwen.code.cli.utils;

import java.util.concurrent.TimeUnit;

import org.apache.commons.lang3.Validate;

public class Timeout {
    private final Long value;
    private final TimeUnit unit;
    public Timeout(Long value, TimeUnit unit) {
        Validate.notNull(value, "value can not be null");
        Validate.notNull(unit, "unit can not be null");
        this.value = value;
        this.unit = unit;
    }

    public Long getValue() {
        return value;
    }

    public TimeUnit getUnit() {
        return unit;
    }

    public static final Timeout TIMEOUT_60_SECONDS = new Timeout(60L, TimeUnit.SECONDS);
    public static final Timeout TIMEOUT_30_MINUTES = new Timeout(60L, TimeUnit.MINUTES);
}
