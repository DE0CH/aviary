import type { HighlightToken } from "@getpaseo/highlight";
import { loadHighlighter } from "@/utils/highlight-grammars";

/** JavaScript time per slice; the rest of the frame stays free for input and rendering. */
const SLICE_BUDGET_MS = 12;
const PLAIN_LINES_PER_UNIT = 2000;

type LineSteps = Generator<HighlightToken[][] | undefined, HighlightToken[][], void>;

/**
 * Splits `content` into source lines, highlighted or plain, a time slice per task so a large file
 * never blocks the JavaScript thread. Highlighting reports `firstLines` lines highlighted, the
 * rest plain, before the whole file; it starts once the highlighter and the file's grammar have
 * arrived (utils/highlight-grammars.ts), plain lines if they cannot. Returns a cancel function.
 */
export function buildSourceLines(
  input: {
    content: string;
    filename: string;
    presentation: "highlighted" | "plain";
    firstLines: number;
  },
  onLines: (lines: HighlightToken[][]) => void,
): () => void {
  if (input.presentation !== "highlighted") {
    return runSteps(plainLinesInSteps(input.content), onLines);
  }
  let cancelled = false;
  let cancelSteps = () => {};
  void loadHighlighter()
    .then(async (highlighter) => {
      await highlighter.loadGrammarForFile(input.filename);
      return highlighter.highlightCodeInSteps(input.content, input.filename, {
        firstLines: input.firstLines,
      });
    })
    .catch(() => plainLinesInSteps(input.content))
    .then((steps) => {
      if (!cancelled) cancelSteps = runSteps(steps, onLines);
    });
  return () => {
    cancelled = true;
    cancelSteps();
  };
}

function runSteps(steps: LineSteps, onLines: (lines: HighlightToken[][]) => void): () => void {
  let cancelSlice = scheduleSlice(runSlice);
  function runSlice() {
    const deadline = performance.now() + SLICE_BUDGET_MS;
    do {
      const step = steps.next();
      if (step.done) {
        onLines(step.value);
        return;
      }
      if (step.value) onLines(step.value);
    } while (performance.now() < deadline);
    cancelSlice = scheduleSlice(runSlice);
  }
  return () => cancelSlice();
}

/**
 * Runs `slice` once the JavaScript thread is idle. React Native runs idle callbacks after pending
 * events and renders without waiting for a frame; a zero timeout waits for the next frame.
 */
function scheduleSlice(slice: () => void): () => void {
  if (typeof requestIdleCallback === "function") {
    const id = requestIdleCallback(slice);
    return () => cancelIdleCallback(id);
  }
  const timer = setTimeout(slice, 0);
  return () => clearTimeout(timer);
}

function* plainLinesInSteps(content: string): LineSteps {
  const lines: HighlightToken[][] = [];
  let from = 0;
  for (;;) {
    const end = content.indexOf("\n", from);
    lines.push([{ text: content.slice(from, end === -1 ? content.length : end), style: null }]);
    if (end === -1) return lines;
    from = end + 1;
    if (lines.length % PLAIN_LINES_PER_UNIT === 0) yield;
  }
}
