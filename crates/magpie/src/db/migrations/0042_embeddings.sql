-- The conversation search's vectors (Deyao, 2026-11-17): one per piece of a user or assistant message's text, of one
-- embedding model, made in the conversation's sandbox. pgvector must be in the database: the deployment creates the
-- extension as its superuser (magpie's role cannot), and this statement then only finds it.
create extension if not exists vector;

create table embeddings (
  conversation_id uuid not null references conversations on delete cascade,
  seq             bigint not null,        -- the message's event
  chunk           integer not null,       -- the piece's number in the message (search::pieces)
  model           text not null,          -- the embedding model the vector is of
  person          text not null,          -- the conversation's owner (a search reads only its own)
  piece_start     integer not null,       -- the piece's byte range in events.text
  piece_end       integer not null,
  vec             vector(1024) not null,
  primary key (conversation_id, seq, chunk, model)
);
create index embeddings_near   on embeddings using hnsw (vec vector_cosine_ops);
create index embeddings_person on embeddings (person, model);
