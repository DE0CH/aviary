---
name: report
description: How to lay out a report, brief, or any document the staff member will send or print: a cover page, a summary with the key figures, numbered sections, charts and tables, and a check of every page before sending. Use it whenever someone asks for a report, a 報告, a 簡報, a summary document, or a PDF to share.
---

# Reports

A report is a full document, not a few notes on pages. Make it in this order, then look at it.

## The pages

- **Cover page**: the title, what period it covers, the date, and `YYC 元意顧問`. Nothing else on it.
- **Summary**: one short page of what matters, with the key figures as a few large numbers rather than a sentence.
- **Numbered sections**: 一、二、三…, each starting on its own line; the analysis goes in full sentences, the
  numbers in tables.
- **Charts and tables** wherever the data has them; read the `dataviz` skill first and let it choose the form and
  the colours.
- **Footer**: `YYC 元意顧問`, the report's name and the page number (`第 N 頁，共 M 頁`).

A full report it is: only when the person asks for a brief (簡報, 快報) is it short — one or two pages, with
the cover as a header block on the first page instead of a page of its own.

## How to build it

Write HTML and CSS and render it with WeasyPrint; the fonts, the Latin-first font list and the reason are in your
instructions. Set the page with `@page { size: A4; margin: 18mm 16mm; }` and let content flow; break pages only
where a section must start on a new page.

```sh
python3 -c "from weasyprint import HTML; HTML('/tmp/report/report.html').write_pdf('/workspace/CEHK_報告.pdf')"
```

## Before sending

Render every page to an image and look at every page, one by one:

```sh
pdftoppm -r 80 -png "/workspace/CEHK_報告.pdf" /tmp/page && ls /tmp/page-*.png
```

Read each image, and fix what looks wrong: text running off the page, a chart too small to read, a heading alone
at the bottom of a page, a figure whose labels overlap, English and Chinese running together without a space. Render
again and look again; only then send the PDF, with a caption that says what it is and how many pages.
