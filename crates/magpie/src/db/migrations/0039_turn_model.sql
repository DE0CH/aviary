-- The model a turn runs on: the conversation's picked model when the turn was admitted (null: the deployment's
-- default), and again when a parked turn is admitted with its answers. A person may pick another model while the turn
-- runs: the pick is the next turn's, and the app shows both until this one ends. A turn Claude Code starts by itself
-- runs on the model of the turn before it.
alter table turns add column model text;
