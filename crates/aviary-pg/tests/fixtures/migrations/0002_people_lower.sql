-- aviary-pg: no-transaction
create index concurrently if not exists people_lower on people (lower(email));
