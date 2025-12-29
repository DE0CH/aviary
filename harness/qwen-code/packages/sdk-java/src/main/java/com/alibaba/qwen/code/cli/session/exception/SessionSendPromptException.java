package com.alibaba.qwen.code.cli.session.exception;

public class SessionSendPromptException extends Exception {
    public SessionSendPromptException() {
    }

    public SessionSendPromptException(String message) {
        super(message);
    }

    public SessionSendPromptException(String message, Throwable cause) {
        super(message, cause);
    }

    public SessionSendPromptException(Throwable cause) {
        super(cause);
    }

    public SessionSendPromptException(String message, Throwable cause, boolean enableSuppression, boolean writableStackTrace) {
        super(message, cause, enableSuppression, writableStackTrace);
    }
}
