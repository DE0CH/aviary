-- A parked turn (DECISIONS 17): the supervisor released its sandbox while it waited only on its open questions. It is
-- neither ended nor failed; the person's answers resume it in a new sandbox.
--   resumed_at        when the answers re-admitted it (the driver's 15 min / 45 min count from here, not admission)
--   parked_ms         the working time before it was parked (the supervisor's durationMs, its question waits left out)
--   parked_asks       the asks it waits on (their answers become the resumed turn's message)
--   answer_tool_uses  the tool calls the resumed turn's answer line answers, until that line is seen (magpie 6.5)
alter table turns drop constraint turns_state_check;
alter table turns add constraint turns_state_check check (state in
  ('admitted','preparing','starting','running','stopping','parked','completed','failed','canceled'));
alter table turns add column resumed_at timestamptz,
                  add column parked_ms bigint,
                  add column parked_asks text[],
                  add column answer_tool_uses text[];
