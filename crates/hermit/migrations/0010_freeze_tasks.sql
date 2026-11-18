-- Freezing (jarvis4 docs/PLAN-fabric.md): a freeze is a resumable row task like a start or a migration. A thaw is a
-- start (its create step thaws the frozen sandbox), so it needs no task of its own.
alter table sandboxes drop constraint sandboxes_task_check;
alter table sandboxes add constraint sandboxes_task_check
  check (task in ('start','migrate','release','freeze'));
