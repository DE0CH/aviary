-- Freezing (jarvis4 docs/PLAN-fabric.md): a freeze and a thaw are resumable row tasks like a start or a migration.
alter table sandboxes drop constraint sandboxes_task_check;
alter table sandboxes add constraint sandboxes_task_check
  check (task in ('start','migrate','release','freeze','thaw'));
