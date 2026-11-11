# Local AI web search

FocalDesk exposes web search as the read-only `web_search` agent tool. It does
not give the model arbitrary network access: the tool talks only to a local
SearXNG HTTP endpoint and returns no fetched-page content. Results are bounded
to eight entries containing a title, URL, short snippet, source engine, and
published date when available.

## Configure SearXNG

Run SearXNG locally and bind it to loopback only. The default endpoint is
`http://127.0.0.1:8080`. To use another local endpoint, create a systemd user
drop-in for `focaldesk-server.service`:

```ini
[Service]
Environment=FOCALDESK_SEARXNG_URL=http://127.0.0.1:8080
```

Then reload user units and restart FocalDesk Server:

```sh
systemctl --user daemon-reload
systemctl --user restart focaldesk-server.service
```

`FOCALDESK_SEARXNG_URL` accepts only unauthenticated `http` loopback URLs
(`localhost`, `127.0.0.1`, or `[::1]`) with no query or fragment. A remote
endpoint, embedded credentials, redirects, result-page fetching, and page
automation are deliberately unsupported.

## Use it

In the AI Console chat, start an explicit request with a phrase such as
“search the web for …”, “web search: …”, or “look up …”. The console queries
the same bounded local SearXNG endpoint directly and returns its result titles,
links, publication dates when provided by the source (otherwise it says so),
snippets, and source engines. The configured SearXNG engine set is used: it
can include Google by default, while still returning results from other sources
when Google blocks an automated query.

When a local model replies that it cannot answer or lacks enough information,
the console automatically performs that same bounded web search using the
original question and replaces the non-answer with relevant linked results.
This fallback stays local-first: only the loopback SearXNG service contacts the
internet, and no result page is fetched by FocalDesk.

The built-in Desktop Assistant includes `web_search`. Select **Desktop
Assistant** in the AI Console and ask it to search the web. Through Ambient
Voice, route the request to that agent, for example: “Hello FocalDesk, ask
agent desktop to search the web for local SearXNG documentation.” The response
should cite the returned result URLs. If SearXNG is not running, the tool fails
closed and the assistant reports that local search is unavailable.
