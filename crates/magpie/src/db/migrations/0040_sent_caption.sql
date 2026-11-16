-- Rule 6 keeps a send's caption on the row of the call that sent the files (`sentCaption`, rows::DERIVATION 2). The
-- tool rows made before it get the caption their send kept (`sent_files.caption`, one per send), and so do the
-- copies of those rows a share froze; a row without a caption is the same under both versions and stays as it is.
update rows r
   set item = jsonb_set(r.item, '{sentCaption}', to_jsonb(s.caption)), derivation = 2
  from (select distinct on (conversation_id, tool_use_id) conversation_id, tool_use_id, caption
          from sent_files order by conversation_id, tool_use_id, seq desc) s
 where r.conversation_id = s.conversation_id and r.tool_use_id = s.tool_use_id and r.kind = 'tool'
   and r.item ? 'sentFiles' and s.caption ~ '\S';

update share_rows r
   set item = jsonb_set(r.item, '{sentCaption}', to_jsonb(s.caption))
  from shares h,
       (select distinct on (conversation_id, tool_use_id) conversation_id, tool_use_id, caption
          from sent_files order by conversation_id, tool_use_id, seq desc) s
 where h.id = r.share_id and s.conversation_id = h.conversation_id and s.tool_use_id = r.tool_use_id
   and r.kind = 'tool' and r.item ? 'sentFiles' and s.caption ~ '\S';
