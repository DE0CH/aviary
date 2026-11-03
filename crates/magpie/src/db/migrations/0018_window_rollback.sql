-- The switch window's rollback before the gate (DESIGN 9.10 step 1): `cutover rollback flip` sets it, and while it is
-- set pigeon routes creates through its legacy upstream (born legacy) and tees uploads into the old child again
-- (`GET /v1/legacy/window`). An expand step: one nullable column, dropped with `import_control` in A6.
alter table import_control add column rollback_at timestamptz;
