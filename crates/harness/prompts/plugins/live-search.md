---
id: live-search
title: Live web and image search
tools: [web_search, page_fetch, image_search]
platforms: [desktop, mobile]
phases: [measure, teach]
use_when: The section needs current or visual material throughout, not one verified fact: fashion and design (reference images, current collections), fast-moving tools and threats, recent events.
avoid_when: Stable subjects. The standard `research_request` already verifies single facts in the background with a cited brief.
cost: Search results take a lot of context; each call adds several hundred tokens.
---
## Plugin: live search

You can search directly while teaching, and see the results yourself.

- `web_search { query, recency? }` returns titles, URLs and snippets.
- `page_fetch { url }` returns readable text.
- `image_search { query }` returns images with their source page and caption. The app shows the images you choose to the learner.

Rules:
- A snippet is not evidence. Fetch the page before you rely on it, and cite it.
- Label an image only from its source's own caption or metadata, never from how it looks to you.
- Single facts still go to `research_request`; it is cheaper and keeps your context clean. Use live search when the learner needs to *see* or *browse* material.
