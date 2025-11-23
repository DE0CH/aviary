export function serializeJsonLine(message: unknown): string {
  try {
    return JSON.stringify(message) + '\n';
  } catch (error) {
    throw new Error(
      `Failed to serialize message to JSON: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
}

export function parseJsonLineSafe(
  line: string,
  context = 'JsonLines',
): unknown | null {
  try {
    return JSON.parse(line);
  } catch (error) {
    console.warn(
      `[${context}] Failed to parse JSON line, skipping:`,
      line.substring(0, 100),
      error instanceof Error ? error.message : String(error),
    );
    return null;
  }
}

export function isValidMessage(message: unknown): boolean {
  return (
    message !== null &&
    typeof message === 'object' &&
    'type' in message &&
    typeof (message as { type: unknown }).type === 'string'
  );
}

export async function* parseJsonLinesStream(
  lines: AsyncIterable<string>,
  context = 'JsonLines',
): AsyncGenerator<unknown, void, unknown> {
  for await (const line of lines) {
    // Skip empty lines
    if (line.trim().length === 0) {
      continue;
    }

    // Parse with error handling
    const message = parseJsonLineSafe(line, context);

    // Skip malformed messages
    if (message === null) {
      continue;
    }

    // Validate message structure
    if (!isValidMessage(message)) {
      console.warn(
        `[${context}] Invalid message structure (missing 'type' field), skipping:`,
        line.substring(0, 100),
      );
      continue;
    }

    yield message;
  }
}
