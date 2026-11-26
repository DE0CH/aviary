---
name: web
description: Search the web and read web pages, search social platforms (小紅書, 抖音, 微博, X, Threads, YouTube, Instagram, Reddit), and run AnyAPI's read-only data APIs (Facebook, LinkedIn, Google Maps and more). Use it for anything current or factual, coverage of a person, company or event, what people say online, or any web page or link the user gives.
---

# Web, social and data APIs

These tools are an HTTP API on your computer's network. Call each one with `curl` (a JSON body of its arguments) and
read the answer: JSON, or plain text for pages and search results. Save long answers to a file in /tmp and read the
part you need.

```sh
curl -sS -H 'Content-Type: application/json' http://tools/v1/tools/web_search \
  -d '{"query": "李家超 施政報告", "num_results": 10}'
```

| Tool | For | Arguments |
|---|---|---|
| `web_search` | titles, links, dates and excerpts from the open web; for Hong Kong topics search in Chinese too | `query`; `num_results`, `include_domains`, `exclude_domains`, `category`, `since`, `until` (YYYY-MM-DD) |
| `web_fetch` | one page's text (an article, notice, PDF link) | `url`; `max_chars` (default 20000), `fresh: true` (the live page), `original: true`, `raw_html: true` |
| `social_search` | the newest public posts on one platform for a keyword; each search costs money, so search the platforms that matter | `platform` (xiaohongshu, douyin, weibo, x, threads, youtube, instagram, reddit), `query`; `limit` |
| `anyapi_search` | find a data API in AnyAPI's catalogue (Facebook posts and comments, LinkedIn, Google Maps, reviews …) | `query`; `platform`, `limit` |
| `anyapi_describe` | one API's exact input fields, output and price (free) | `api` |
| `anyapi_run` | run it; each run costs its price, so run what the task needs | `api`, `input` (exactly the fields anyapi_describe gives) |
| `anyapi_result` | read a run's result again, free, for about 15 minutes; reshape it | `result_id`; `fields`, `max_items`, `summary`, `jq` |

`curl -sS http://tools/v1/tools` lists every tool with its full description and arguments.

Answers: 200 is the result; 400 means an argument is wrong (`message` says which); 422 is the tool's own error
(`error`: e.g. a page that can't be read). Read pages with `web_fetch`: it returns the text of an article cleanly.
Your computer also reaches most web sites directly (`curl`, for downloads and other sites' APIs). A site that is
blocked there is read with `web_fetch`.
A video link (YouTube, Now, ViuTV, TVB) is the `youtube` skill: `web_fetch` reads only its page.
