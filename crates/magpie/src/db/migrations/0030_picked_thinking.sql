-- The thinking level a person picks for a conversation (M5 `thinking`): its turns run at it from the next one
-- (`start_turn.thinking`). Null: the level the models route names for the conversation's model.
alter table conversations add column picked_thinking text;
