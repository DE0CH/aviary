---
name: newswatch
description: YYC's news and social-media monitoring data for Hong Kong and Greater China: full-text search of collected posts and articles (Chinese and English), what each monitoring stream filed on a day, the 數據分析 views (hot topics, people, themes, sources, trends, daily scores), daily summaries and reports, and WiseSearch (慧科, the archive of HK and Greater China newspapers and media). Use it for monitoring, coverage, sentiment, media analysis and press research.
---

# News monitoring data

The data is an HTTP API on your computer's network, one call per tool (prefix `data_`): `curl` with a JSON body of
the tool's arguments; the answer is JSON. Save big answers to a file in /tmp and read what you need with `jq` or Python.

```sh
curl -sS -H 'Content-Type: application/json' http://tools/v1/tools/data_posts_search \
  -d '{"query": "施政報告", "from": "2026-10-01"}' > /tmp/posts.json
```

Start with `data_streams_list` (the subjects being monitored and their ids), then:

| Tool | For |
|---|---|
| `data_posts_search` | full-text search (繁/简/English) with stream, score, dates, types, outlets, regions, platforms |
| `data_post_get` | one post in full: text, transcripts, comments, per-stream scores and reasons |
| `data_stream_items` | what a stream filed on a day (串流) |
| `data_analysis_posts`, `data_analysis_hot_topics`, `data_analysis_topic`, `data_analysis_person`, `data_analysis_themes` | 數據分析: posts with facets, hot topics, one topic, a person, themes |
| `data_analysis_overview`, `data_analysis_authors`, `data_analysis_sources`, `data_analysis_trend`, `data_analysis_terms` | 數據分析: summary and daily scores, authors, sources, trend, hot terms |
| `data_daily_summary`, `data_reports_list`, `data_report_file` | the daily AI summary; daily report files |
| `data_sources_list` | every outlet and platform collected |
| `data_wisers_search`, `data_wisers_job`, `data_wisers_doc` | WiseSearch: a search runs as a job (`data_wisers_job` until done), then full documents. Searches and document opens cost the shared account's quota: search deliberately and give `reason` |
| `data_wisers_facets`, `data_wisers_options`, `data_wisers_labels`, `data_wisers_media`, `data_wisers_log` | a search's facets; options, labels, media lookup, the usage log (free) |

`curl -sS http://tools/v1/tools` gives each tool's full description and arguments.

Every post, topic, person, theme, source and report in an answer carries `dashboard_url`, its page on the monitoring
site: link it when you cite or mention the item (with the original `url` for a post). Analysis counts depend on which
sources and keywords are monitored; say so when you report numbers. Dates are Hong Kong time.

Answers: 200 is the result; 400 means an argument is wrong (`message`); 422 is the tool's own error (`error`).
