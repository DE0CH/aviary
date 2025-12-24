package com.alibaba.qwen.code.cli.transport.process;

import com.alibaba.qwen.code.cli.transport.TransportOptions;

import org.apache.commons.lang3.StringUtils;

import java.io.File;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;

class TransportOptionsAdapter {
    TransportOptions transportOptions;
    private static final Long DEFAULT_TURN_TIMEOUT_MS = 1000 * 60 * 30L;
    private static final Long DEFAULT_MESSAGE_TIMEOUT_MS = 1000 * 60 * 3L;

    TransportOptionsAdapter(TransportOptions userTransportOptions) {
        transportOptions = addDefaultTransportOptions(userTransportOptions);
    }

    TransportOptions getHandledTransportOptions() {
        return transportOptions;
    }

    String getCwd() {
        return transportOptions.getCwd();
    }

    String[] buildCommandArgs() {
        List<String> args = new ArrayList<>(
                Arrays.asList(transportOptions.getPathToQwenExecutable(), "--input-format", "stream-json", "--output-format",
                        "stream-json", "--channel=SDK"));

        if (StringUtils.isNotBlank(transportOptions.getModel())) {
            args.add("--model");
            args.add(transportOptions.getModel());
        }

        if (transportOptions.getPermissionMode() != null) {
            args.add("--permission-mode");
            args.add(transportOptions.getPermissionMode().getValue());
        }

        if (transportOptions.getMaxSessionTurns() != null) {
            args.add("--max-session-turns");
            args.add(transportOptions.getMaxSessionTurns().toString());
        }

        if (transportOptions.getCoreTools() != null && !transportOptions.getCoreTools().isEmpty()) {
            args.add("--core-tools");
            args.add(String.join(",", transportOptions.getCoreTools()));
        }

        if (transportOptions.getExcludeTools() != null && !transportOptions.getExcludeTools().isEmpty()) {
            args.add("--exclude-tools");
            args.add(String.join(",", transportOptions.getExcludeTools()));
        }

        if (transportOptions.getAllowedTools() != null && !transportOptions.getAllowedTools().isEmpty()) {
            args.add("--allowed-tools");
            args.add(String.join(",", transportOptions.getAllowedTools()));
        }

        if (StringUtils.isNotBlank(transportOptions.getAuthType())) {
            args.add("--auth-type");
            args.add(transportOptions.getAuthType());
        }

        if (transportOptions.getIncludePartialMessages() != null && transportOptions.getIncludePartialMessages()) {
            args.add("--include-partial-messages");
        }
        return args.toArray(new String[] {});
    }

    private TransportOptions addDefaultTransportOptions(TransportOptions userTransportOptions) {
        TransportOptions transportOptions = userTransportOptions.clone();

        if (StringUtils.isBlank(transportOptions.getPathToQwenExecutable())) {
            transportOptions.setPathToQwenExecutable("qwen");
        }

        if (StringUtils.isBlank(transportOptions.getCwd())) {
            transportOptions.setCwd(new File("").getAbsolutePath());
        }

        Map<String, String> env = new HashMap<>(System.getenv());
        Optional.ofNullable(transportOptions.getEnv()).ifPresent(env::putAll);
        transportOptions.setEnv(env);

        if (transportOptions.getTurnTimeoutMs() == null) {
            transportOptions.setTurnTimeoutMs(DEFAULT_TURN_TIMEOUT_MS);
        }

        if (transportOptions.getMessageTimeoutMs() == null) {
            transportOptions.setMessageTimeoutMs(DEFAULT_MESSAGE_TIMEOUT_MS);
        }
        return transportOptions;
    }
}
