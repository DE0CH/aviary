import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/cpp";

// C / C++ / Objective-C
const cpp: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  c: cpp,
  h: cpp,
  cc: cpp,
  cpp,
  cxx: cpp,
  hpp: cpp,
  hxx: cpp,
  m: cpp,
  mm: cpp,
};
